use std::collections::{HashMap, HashSet};

use serde::Serialize;

use grapha_core::graph::{Edge, EdgeKind, Graph, Node, NodeKind};

use crate::localization::{
    LocalizationCatalogIndex, LocalizationCatalogRecord, LocalizationReference, directory_distance,
    localization_usage_nodes, node_index, parse_usage_reference, parse_wrapper_binding,
    wrapper_name_matches_catalog_key,
};

use super::SymbolInfo;
use super::l10n::{contains_parents, to_symbol_info, ui_path};

#[derive(Debug, Serialize)]
pub struct UsagesResult {
    pub query: UsageQuery,
    pub records: Vec<RecordUsages>,
}

#[derive(Debug, Serialize)]
pub struct UsageQuery {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecordUsages {
    pub record: LocalizationCatalogRecord,
    pub usages: Vec<UsageSite>,
}

#[derive(Debug, Serialize)]
pub struct UsageSite {
    pub owner: SymbolInfo,
    pub view: SymbolInfo,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ui_path: Vec<String>,
    pub reference: LocalizationReference,
}

pub fn query_usages(
    graph: &Graph,
    catalogs: &LocalizationCatalogIndex,
    key: &str,
    table: Option<&str>,
) -> UsagesResult {
    let (records, matched_by, resolved_key) = resolve_records(catalogs, key, table);

    if records.is_empty() {
        return UsagesResult {
            query: UsageQuery {
                key: key.to_string(),
                table: table.map(ToString::to_string),
                matched_by,
                resolved_key,
            },
            records: Vec::new(),
        };
    }

    let node_index = node_index(graph);
    let parents = contains_parents(graph);
    let edges_by_target = edges_by_target(graph);

    let target_keys: HashSet<(&str, &str)> = records
        .iter()
        .map(|r| (r.table.as_str(), r.key.as_str()))
        .collect();
    let target_key_strs: HashSet<&str> = records.iter().map(|r| r.key.as_str()).collect();

    let wrapper_by_binding = wrapper_nodes_by_binding(&node_index);

    let target_wrapper_ids: HashSet<&str> = target_keys
        .iter()
        .flat_map(|(t, k)| wrapper_by_binding.get(&(*t, *k)).into_iter().flatten())
        .map(|n| n.id.as_str())
        .collect();

    let target_wrapper_names: HashSet<&str> = target_keys
        .iter()
        .flat_map(|(t, k)| wrapper_by_binding.get(&(*t, *k)).into_iter().flatten())
        .map(|n| n.name.as_str())
        .collect();

    let usage_nodes = localization_usage_nodes(graph);

    let mut candidate_ids: HashSet<&str> = HashSet::new();

    for node in &usage_nodes {
        if let (Some(t), Some(k)) = (
            node.metadata.get("l10n.table"),
            node.metadata.get("l10n.key"),
        ) && target_keys.contains(&(t.as_str(), k.as_str()))
        {
            candidate_ids.insert(node.id.as_str());
            continue;
        }

        if let Some(literal) = node.metadata.get("l10n.literal")
            && target_key_strs.contains(literal.as_str())
        {
            candidate_ids.insert(node.id.as_str());
            continue;
        }

        if let Some(wrapper_sym) = node.metadata.get("l10n.wrapper_symbol")
            && target_wrapper_ids.contains(wrapper_sym.as_str())
        {
            candidate_ids.insert(node.id.as_str());
            continue;
        }

        if let Some(wrapper_name) = node.metadata.get("l10n.wrapper_name")
            && (target_wrapper_names.contains(wrapper_name.as_str())
                || records
                    .iter()
                    .any(|record| wrapper_name_matches_catalog_key(wrapper_name, &record.key)))
        {
            candidate_ids.insert(node.id.as_str());
        }
    }

    for (t, k) in &target_keys {
        let Some(wrappers) = wrapper_by_binding.get(&(*t, *k)) else {
            continue;
        };
        for wrapper in wrappers {
            let Some(edges) = edges_by_target.get(wrapper.id.as_str()) else {
                continue;
            };
            for edge in edges {
                match edge.kind {
                    EdgeKind::TypeRef => {
                        candidate_ids.insert(edge.source.as_str());
                    }
                    EdgeKind::Implements => {
                        if let Some(impl_edges) = edges_by_target.get(edge.source.as_str()) {
                            for impl_edge in impl_edges {
                                if impl_edge.kind == EdgeKind::TypeRef {
                                    candidate_ids.insert(impl_edge.source.as_str());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let edges_by_source = edges_by_source_for(graph, &candidate_ids);

    let mut record_groups = Vec::new();
    for record in records {
        let mut usages: Vec<UsageSite> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut owner_ids: HashSet<String> = HashSet::new();

        for node in &usage_nodes {
            if !candidate_ids.contains(node.id.as_str()) {
                continue;
            }
            let Some(reference) = resolve_for_record(
                node,
                &record,
                &edges_by_source,
                &node_index,
                &wrapper_by_binding,
                catalogs,
            ) else {
                continue;
            };

            if !seen.insert(node.id.clone()) {
                continue;
            }

            let owner = owning_symbol(node.id.as_str(), &parents, &node_index).unwrap_or(node);
            owner_ids.insert(owner.id.clone());
            usages.push(UsageSite {
                owner: to_symbol_info(owner),
                view: to_symbol_info(node),
                ui_path: ui_path(node.id.as_str(), owner.id.as_str(), &parents, &node_index),
                reference,
            });
        }

        let wrapper_usages = wrapper_caller_usages(
            &record,
            &wrapper_by_binding,
            &edges_by_target,
            &node_index,
            &parents,
        );
        for usage in wrapper_usages {
            if owner_ids.contains(usage.view.id.as_str()) {
                continue;
            }
            if seen.insert(usage.view.id.clone()) {
                usages.push(usage);
            }
        }
        record_groups.push(RecordUsages { record, usages });
    }

    dedup_by_catalog_proximity(&mut record_groups);

    UsagesResult {
        query: UsageQuery {
            key: key.to_string(),
            table: table.map(ToString::to_string),
            matched_by,
            resolved_key,
        },
        records: record_groups,
    }
}

fn resolve_for_record(
    usage_node: &Node,
    record: &LocalizationCatalogRecord,
    edges_by_source: &HashMap<&str, Vec<&Edge>>,
    node_index: &HashMap<&str, &Node>,
    wrapper_by_binding: &HashMap<(&str, &str), Vec<&Node>>,
    catalogs: &LocalizationCatalogIndex,
) -> Option<LocalizationReference> {
    let base_ref = parse_usage_reference(usage_node)?;

    if let (Some(table), Some(key)) = (base_ref.table.as_deref(), base_ref.key.as_deref())
        && table == record.table
        && key == record.key
    {
        return Some(base_ref);
    }

    if let Some(literal) = base_ref.literal.as_deref()
        && literal == record.key
        && base_ref.table.is_none()
        && base_ref.key.is_none()
    {
        let literal_records = if let Some(table) = base_ref.table.as_deref() {
            catalogs.records_for(table, literal)
        } else {
            catalogs.records_for_key(literal)
        };
        if literal_records
            .iter()
            .any(|r| r.table == record.table && r.key == record.key)
        {
            let mut reference = base_ref.clone();
            reference.table = Some(record.table.clone());
            reference.key = Some(record.key.clone());
            return Some(reference);
        }
    }

    if let Some(edges) = edges_by_source.get(usage_node.id.as_str()) {
        for edge in edges {
            if edge.kind != EdgeKind::TypeRef {
                continue;
            }
            let Some(wrapper_node) = node_index.get(edge.target.as_str()).copied() else {
                continue;
            };
            let Some(binding) = parse_wrapper_binding(wrapper_node) else {
                continue;
            };
            if binding.table == record.table && binding.key == record.key {
                let mut reference = base_ref.clone();
                reference.wrapper_symbol = Some(wrapper_node.id.clone());
                reference.table = Some(binding.table);
                reference.key = Some(binding.key);
                if reference.fallback.is_none() {
                    reference.fallback = binding.fallback;
                }
                if reference.arg_count.is_none() {
                    reference.arg_count = binding.arg_count;
                }
                return Some(reference);
            }
        }
    }

    if let Some(wrapper_name) = base_ref.wrapper_name.as_deref() {
        let target_wrappers = wrapper_by_binding
            .get(&(record.table.as_str(), record.key.as_str()))
            .map(Vec::as_slice)
            .unwrap_or_default();

        for wrapper_node in target_wrappers {
            if wrapper_node.name == wrapper_name
                || wrapper_node_matches_name(
                    wrapper_node,
                    wrapper_name,
                    base_ref.wrapper_base.as_deref(),
                )
            {
                let Some(binding) = parse_wrapper_binding(wrapper_node) else {
                    continue;
                };
                let mut reference = base_ref.clone();
                reference.wrapper_symbol = Some(wrapper_node.id.clone());
                reference.table = Some(binding.table);
                reference.key = Some(binding.key);
                if reference.fallback.is_none() {
                    reference.fallback = binding.fallback;
                }
                if reference.arg_count.is_none() {
                    reference.arg_count = binding.arg_count;
                }
                return Some(reference);
            }
        }

        if wrapper_name_matches_catalog_key(wrapper_name, &record.key) {
            let mut reference = base_ref.clone();
            reference.table = Some(record.table.clone());
            reference.key = Some(record.key.clone());
            return Some(reference);
        }
    }

    None
}

fn wrapper_node_matches_name(node: &Node, wrapper_name: &str, wrapper_base: Option<&str>) -> bool {
    if let Some(base) = wrapper_base
        && !node.id.contains(base)
    {
        return false;
    }
    if node.name == wrapper_name {
        return true;
    }
    let node_tokens = name_tokens(&node.name);
    let target_tokens = name_tokens(wrapper_name);
    !node_tokens.is_empty() && node_tokens == target_tokens
}

fn name_tokens(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;

    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            if !current.is_empty() {
                tokens.push(current.to_ascii_lowercase());
                current.clear();
            }
            previous = None;
            continue;
        }

        let starts_new_token = previous.is_some_and(|prev| {
            (prev.is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (prev.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (prev.is_ascii_digit() && ch.is_ascii_alphabetic())
        });
        if starts_new_token && !current.is_empty() {
            tokens.push(current.to_ascii_lowercase());
            current.clear();
        }

        current.push(ch);
        previous = Some(ch);
    }

    if !current.is_empty() {
        tokens.push(current.to_ascii_lowercase());
    }

    tokens.sort();
    tokens
}

fn resolve_records(
    catalogs: &LocalizationCatalogIndex,
    input: &str,
    table: Option<&str>,
) -> (
    Vec<LocalizationCatalogRecord>,
    Option<String>,
    Option<String>,
) {
    let by_key = if let Some(table) = table {
        catalogs.records_for(table, input)
    } else {
        catalogs.records_for_key(input)
    };
    if !by_key.is_empty() {
        return (by_key, None, None);
    }

    let by_value = catalogs.records_for_value(input);
    if !by_value.is_empty() {
        let resolved_key = by_value[0].key.clone();
        return (by_value, Some("value".to_string()), Some(resolved_key));
    }

    (Vec::new(), None, None)
}

fn dedup_by_catalog_proximity(record_groups: &mut [RecordUsages]) {
    let mut multi_catalog: HashMap<(String, String), Vec<(String, usize)>> = HashMap::new();
    for (index, group) in record_groups.iter().enumerate() {
        multi_catalog
            .entry((group.record.table.clone(), group.record.key.clone()))
            .or_default()
            .push((group.record.catalog_dir.clone(), index));
    }

    for catalog_entries in multi_catalog.values() {
        if catalog_entries.len() < 2 {
            continue;
        }
        let catalog_dirs: Vec<&str> = catalog_entries
            .iter()
            .map(|(dir, _)| dir.as_str())
            .collect();
        for &(ref this_dir, group_idx) in catalog_entries {
            record_groups[group_idx].usages.retain(|usage| {
                let usage_file = std::path::Path::new(usage.view.file.as_str());
                let this_distance = directory_distance(usage_file, this_dir);
                !catalog_dirs.iter().any(|&other_dir| {
                    other_dir != this_dir.as_str()
                        && directory_distance(usage_file, other_dir) < this_distance
                })
            });
        }
    }
}

fn edges_by_target(graph: &Graph) -> HashMap<&str, Vec<&Edge>> {
    let mut map: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        map.entry(edge.target.as_str()).or_default().push(edge);
    }
    map
}

fn edges_by_source_for<'a>(
    graph: &'a Graph,
    node_ids: &HashSet<&str>,
) -> HashMap<&'a str, Vec<&'a Edge>> {
    let mut map: HashMap<&'a str, Vec<&'a Edge>> = HashMap::new();
    for edge in &graph.edges {
        if node_ids.contains(edge.source.as_str()) {
            map.entry(edge.source.as_str()).or_default().push(edge);
        }
    }
    map
}

fn wrapper_nodes_by_binding<'a>(
    node_index: &HashMap<&str, &'a Node>,
) -> HashMap<(&'a str, &'a str), Vec<&'a Node>> {
    let mut map: HashMap<(&'a str, &'a str), Vec<&'a Node>> = HashMap::new();
    for node in node_index.values().copied() {
        if parse_wrapper_binding(node).is_some()
            && let (Some(table_val), Some(key_val)) = (
                node.metadata.get("l10n.wrapper.table"),
                node.metadata.get("l10n.wrapper.key"),
            )
        {
            map.entry((table_val.as_str(), key_val.as_str()))
                .or_default()
                .push(node);
        }
    }
    map
}

fn wrapper_caller_usages<'a>(
    record: &LocalizationCatalogRecord,
    wrapper_by_binding: &HashMap<(&str, &str), Vec<&'a Node>>,
    edges_by_target: &HashMap<&str, Vec<&'a Edge>>,
    node_index: &HashMap<&'a str, &'a Node>,
    parents: &HashMap<&'a str, &'a str>,
) -> Vec<UsageSite> {
    let matching_wrappers = wrapper_by_binding
        .get(&(record.table.as_str(), record.key.as_str()))
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut usages = Vec::new();
    for wrapper in matching_wrappers {
        let binding = parse_wrapper_binding(wrapper).unwrap();
        let reference = LocalizationReference {
            ref_kind: "wrapper".to_string(),
            wrapper_name: Some(wrapper.name.clone()),
            wrapper_base: None,
            wrapper_symbol: Some(wrapper.id.clone()),
            table: Some(binding.table.clone()),
            key: Some(binding.key.clone()),
            fallback: binding.fallback.clone(),
            arg_count: binding.arg_count,
            literal: None,
        };

        let accessor_ids = wrapper_and_accessor_ids(wrapper.id.as_str(), edges_by_target);
        let accessor_set: HashSet<&str> = accessor_ids.iter().map(String::as_str).collect();

        for target_id in &accessor_ids {
            let Some(incoming) = edges_by_target.get(target_id.as_str()) else {
                continue;
            };
            for edge in incoming {
                if !matches!(edge.kind, EdgeKind::Calls | EdgeKind::Uses) {
                    continue;
                }
                if accessor_set.contains(edge.source.as_str()) {
                    continue;
                }
                let Some(caller) = node_index.get(edge.source.as_str()) else {
                    continue;
                };
                if caller.metadata.contains_key("l10n.ref_kind")
                    || caller.metadata.contains_key("l10n.wrapper.key")
                {
                    continue;
                }
                let owner =
                    owning_symbol(caller.id.as_str(), parents, node_index).unwrap_or(caller);
                usages.push(UsageSite {
                    owner: to_symbol_info(owner),
                    view: to_symbol_info(caller),
                    ui_path: Vec::new(),
                    reference: reference.clone(),
                });
            }
        }
    }
    usages
}

fn wrapper_and_accessor_ids(
    wrapper_id: &str,
    edges_by_target: &HashMap<&str, Vec<&Edge>>,
) -> Vec<String> {
    let mut ids = vec![wrapper_id.to_string()];
    if let Some(incoming) = edges_by_target.get(wrapper_id) {
        for edge in incoming {
            if edge.kind == EdgeKind::Implements {
                ids.push(edge.source.clone());
            }
        }
    }
    ids
}

fn owning_symbol<'a>(
    node_id: &'a str,
    parents: &HashMap<&'a str, &'a str>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Option<&'a Node> {
    let mut current = Some(node_id);
    while let Some(id) = current {
        let node = node_index.get(id).copied()?;
        if !matches!(node.kind, NodeKind::View | NodeKind::Branch)
            && !node.metadata.contains_key("l10n.ref_kind")
        {
            return Some(node);
        }
        current = parents.get(id).copied();
    }
    None
}
