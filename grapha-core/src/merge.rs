use std::collections::{HashMap, HashSet};

use crate::extract::ExtractionResult;
use crate::graph::{EdgeKind, Graph, NodeKind};

struct NameEntry {
    id: String,
    module: Option<String>,
    file: String,
}

struct ResolveContext<'a> {
    raw_target: &'a str,
    source_module: Option<&'a str>,
    source_imports: Option<&'a HashSet<String>>,
    prefix_hint: Option<&'a str>,
    source_owner_names: &'a [String],
    candidate_to_owner_names: &'a HashMap<String, Vec<String>>,
    candidate_file_stems: &'a HashMap<String, String>,
}

fn looks_like_file_path(segment: &str) -> bool {
    segment.contains('/') || segment.ends_with(".rs") || segment.ends_with(".swift")
}

fn target_segments(target: &str) -> Vec<&str> {
    let mut colon_parts: Vec<_> = target.split("::").collect();
    if !colon_parts.is_empty() && looks_like_file_path(colon_parts[0]) {
        colon_parts.remove(0);
    }

    if colon_parts.len() > 1 {
        return colon_parts;
    }

    let mut segments = Vec::new();
    if let Some(single_part) = colon_parts.into_iter().next() {
        segments.extend(single_part.split('.').filter(|segment| !segment.is_empty()));
    }
    if segments.is_empty() {
        segments.push(target);
    }
    segments
}

fn target_symbol_name(target: &str) -> &str {
    let segments = target_segments(target);
    segments.last().copied().unwrap_or(target)
}

fn target_prefix_hint(target: &str) -> Option<String> {
    let segments = target_segments(target);
    if segments.len() < 2 {
        return None;
    }

    let hint = segments[segments.len() - 2];
    if matches!(hint, "crate" | "super") {
        None
    } else {
        Some(hint.to_string())
    }
}

fn should_enforce_hint(target: &str, hint: &str) -> bool {
    hint.eq_ignore_ascii_case("self")
        || target.contains('.')
        || hint.chars().any(|ch| ch.is_ascii_uppercase())
}

fn is_resolvable_symbol_kind(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File
            | NodeKind::Import
            | NodeKind::Export
            | NodeKind::Package
            | NodeKind::Parameter
            | NodeKind::View
            | NodeKind::Branch
    )
}

/// Confidence factor applied to edges bound through a file's `use` imports +
/// the workspace module map. Lower than same-module (0.9) and direct-import
/// single-candidate (0.8) resolution, but high enough to be trusted: the bind
/// only happens when an *actual* import in the source file disambiguates the
/// leading path segment to exactly one canonical definition.
const IMPORT_BOUND_CONFIDENCE: f64 = 0.75;

/// Kinds that can be the *canonical definition* a cross-boundary reference
/// (a type ref, an associated-function call, or a trait impl) points at.
/// Members (methods/fields) are resolved relative to one of these owners, never
/// indexed here directly, so a bare `Type` reference never binds to a method.
fn is_canonical_definition_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::TypeAlias
            | NodeKind::Class
            | NodeKind::Function
            | NodeKind::Constant
    )
}

/// Normalize a crate/module identifier for cross-language matching.
///
/// Rust crate names appear with underscores in source (`use nous_core::…`) but
/// the workspace module map and graph node `module` fields carry the package
/// name with hyphens (`nous-core`). Folding both to a hyphenless, lowercase
/// form makes `nous_core`, `nous-core` and `NousCore` compare equal.
fn normalize_module_key(name: &str) -> String {
    name.chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The first "meaningful" path segment of a reference target — the leading type
/// or crate name. File-path and `crate`/`super`/`self` prefixes are skipped.
fn leading_path_segment(target: &str) -> Option<&str> {
    target_segments(target)
        .into_iter()
        .find(|segment| !matches!(*segment, "crate" | "super" | "self" | "Self"))
}

/// Build, per source file, a map from an imported leading identifier to the set
/// of normalized modules it can resolve to.
///
/// Two identifier flavors are recorded for every `use` statement:
///   * each explicitly imported symbol  (`use nous_core::{Confidence}` →
///     `Confidence` ⇒ {nous-core}), and
///   * the trailing path segment itself  (`use nous_core::Confidence` →
///     `Confidence` ⇒ {nous-core}, and the crate alias `nous_core` ⇒ {nous-core}).
///
/// Only an actual import contributes here, so resolution can never invent a
/// cross-crate edge to an unrelated same-named symbol.
fn build_imported_symbol_modules(
    results: &[ExtractionResult],
) -> HashMap<String, HashMap<String, HashSet<String>>> {
    let mut per_file: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();
    for result in results {
        let Some(first_node) = result.nodes.first() else {
            continue;
        };
        let file_key = first_node.file.to_string_lossy().to_string();
        let file_entry = per_file.entry(file_key).or_default();
        for import in &result.imports {
            record_import(file_entry, import);
        }
    }
    per_file
}

fn record_import(
    file_entry: &mut HashMap<String, HashSet<String>>,
    import: &crate::resolve::Import,
) {
    let raw_path = import.path.strip_prefix("import ").unwrap_or(&import.path);
    let segments: Vec<&str> = raw_path
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect();
    // The crate/module the imported symbols come from. For `use a::b::Sym` the
    // crate is `a`; for a bare `use nous_core;` the crate is `nous_core`.
    let Some(crate_segment) = segments
        .iter()
        .find(|segment| !matches!(**segment, "crate" | "super" | "self"))
    else {
        return;
    };
    let module_key = normalize_module_key(crate_segment);
    if module_key.is_empty() {
        return;
    }

    // Explicit grouped/named symbols: `use a::{A, B as C}` and `use a::Sym`.
    for symbol in &import.symbols {
        if let Some(name) = symbol_binding_name(symbol) {
            file_entry
                .entry(name)
                .or_default()
                .insert(module_key.clone());
        }
    }

    // The path's own trailing segment is also a usable leading identifier
    // (covers `use a::Type;` where `symbols` is empty), plus the crate alias
    // itself for fully-qualified references like `nous_core::Confidence`.
    if let Some(last) = segments.last()
        && *last != "*"
    {
        file_entry
            .entry((*last).to_string())
            .or_default()
            .insert(module_key.clone());
    }
    file_entry
        .entry((*crate_segment).to_string())
        .or_default()
        .insert(module_key);
}

/// Extract the bound name of an imported symbol, honoring `Sym as Alias`.
fn symbol_binding_name(symbol: &str) -> Option<String> {
    let trimmed = symbol.trim();
    if trimmed.is_empty() || trimmed == "*" || trimmed == "self" {
        return None;
    }
    let bound = trimmed
        .rsplit(" as ")
        .next()
        .unwrap_or(trimmed)
        .trim()
        .rsplit("::")
        .next()
        .unwrap_or(trimmed)
        .trim();
    (!bound.is_empty()).then(|| bound.to_string())
}

/// Index canonical type/function definitions by (normalized module, name).
fn build_module_symbol_index<'a>(
    nodes: impl Iterator<Item = (&'a str, NodeKind, Option<&'a str>, &'a str)>,
) -> HashMap<(String, String), Vec<String>> {
    let mut index: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (id, kind, module, name) in nodes {
        if !is_canonical_definition_kind(kind) {
            continue;
        }
        let Some(module) = module else { continue };
        index
            .entry((normalize_module_key(module), name.to_string()))
            .or_default()
            .push(id.to_string());
    }
    index
}

struct ImportBindContext<'a> {
    source_file: &'a str,
    imported_symbol_modules: &'a HashMap<String, HashMap<String, HashSet<String>>>,
    module_symbol_index: &'a HashMap<(String, String), Vec<String>>,
    name_to_entries: &'a HashMap<&'a str, Vec<NameEntry>>,
    candidate_to_owner_names: &'a HashMap<String, Vec<String>>,
    id_to_kind: &'a HashMap<&'a str, NodeKind>,
}

/// Resolve a cross-boundary reference target through the source file's imports
/// and the workspace module map. Returns the canonical node id to bind to.
///
/// Strategy: take the leading path segment (a type or crate), find the unique
/// module it was imported from in this file, then look up the canonical
/// definition there. For associated-function calls (`Type::method`) the trailing
/// method is then located among that type's members; if the method itself can't
/// be pinned the type node is *not* used as a fallback for calls (that would be
/// a wrong call target), so the edge is left for the caller to drop.
fn resolve_via_imports(
    target: &str,
    kind: EdgeKind,
    ctx: &ImportBindContext<'_>,
) -> Option<String> {
    let file_imports = ctx.imported_symbol_modules.get(ctx.source_file)?;
    let leading = leading_path_segment(target)?;
    let module_key = unique_module_for(file_imports.get(leading)?)?;

    // Strip the leading file-path / `crate` / `super` noise, then identify the
    // canonical type/symbol name and any trailing member.
    let meaningful: Vec<&str> = target_segments(target)
        .into_iter()
        .filter(|segment| !matches!(*segment, "crate" | "super" | "self" | "Self"))
        .collect();
    let (type_name, member) = split_type_and_member(&meaningful, leading, file_imports)?;

    let type_id = ctx
        .module_symbol_index
        .get(&(module_key.clone(), type_name.to_string()))
        .and_then(|ids| single(ids))
        .cloned();

    match (kind, member) {
        (EdgeKind::Calls, Some(method)) => {
            // `Type::method` — bind to the method, never the type (a type node
            // is the wrong target for a call). Refuse if the method (e.g. an
            // external/std associated fn) can't be pinned.
            resolve_member(method, type_name, &module_key, ctx)
        }
        (EdgeKind::Calls, None) => {
            // Bare imported function call (`func()` where `use a::func`).
            type_id
        }
        _ => type_id,
    }
}

/// Bind a dangling `impl Trait for Type` target to the canonical trait node.
///
/// Resolution order:
///   1. import-guided binding (trait imported from another crate), then
///   2. a unique same-module trait of that name (covers same-crate, cross-file
///      impls like `impl ProviderTransport for StdioTransport`).
///
/// Returns `None` when the trait can't be uniquely pinned (e.g. external std
/// traits such as `Default`/`From`), so the caller keeps the edge as written.
fn resolve_implements_target(
    target: &str,
    source_module: Option<&str>,
    ctx: &ImportBindContext<'_>,
) -> Option<String> {
    if let Some(bound) = resolve_via_imports(target, EdgeKind::Implements, ctx) {
        return Some(bound);
    }

    // Same-module trait fallback: only bind when exactly one *trait* of that
    // name exists in the source's module (no same-name guessing across crates).
    let trait_name = leading_path_segment(target)?;
    let module = source_module?;
    let ids = ctx
        .module_symbol_index
        .get(&(normalize_module_key(module), trait_name.to_string()))?;
    let traits: Vec<&String> = ids
        .iter()
        .filter(|id| {
            matches!(
                ctx.id_to_kind.get(id.as_str()),
                Some(NodeKind::Trait | NodeKind::Protocol)
            )
        })
        .collect();
    single(&traits).map(|id| (*id).clone())
}

/// From the meaningful (noise-stripped) segments, derive `(type_name, member)`.
///
/// * `[Type]`                       → (Type, None)
/// * `[Type, method]` (imported sym)→ (Type, Some(method))
/// * `[crate_alias, Symbol]`        → (Symbol, None)
/// * `[crate_alias, Symbol, method]`→ (Symbol, Some(method))
fn split_type_and_member<'a>(
    meaningful: &[&'a str],
    leading: &'a str,
    file_imports: &HashMap<String, HashSet<String>>,
) -> Option<(&'a str, Option<&'a str>)> {
    let first = *meaningful.first()?;
    let leading_is_crate = first == leading && is_imported_crate_alias(file_imports, leading);

    if leading_is_crate {
        // crate_alias :: Symbol [:: member]
        let symbol = *meaningful.get(1)?;
        Some((symbol, meaningful.get(2).copied()))
    } else {
        // Type [:: member]
        Some((first, meaningful.get(1).copied()))
    }
}

/// Find the unique method named `method` whose owner type is `type_name` within
/// `module_key`. Uses the precomputed member→owner-name map plus the node's
/// module to avoid same-name collisions across crates.
fn resolve_member(
    method: &str,
    type_name: &str,
    module_key: &str,
    ctx: &ImportBindContext<'_>,
) -> Option<String> {
    let candidates = ctx.name_to_entries.get(method)?;
    let matches: Vec<&String> = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .module
                .as_deref()
                .is_some_and(|module| normalize_module_key(module) == module_key)
        })
        .filter(|candidate| {
            ctx.candidate_to_owner_names
                .get(&candidate.id)
                .is_some_and(|owners| owners.iter().any(|owner| owner == type_name))
        })
        .map(|candidate| &candidate.id)
        .collect();
    single(&matches).map(|id| id.to_string())
}

fn is_imported_crate_alias(file_imports: &HashMap<String, HashSet<String>>, leading: &str) -> bool {
    // A leading segment is a crate alias when its own normalized form is the
    // module it maps to (e.g. `nous_core` ⇒ {nous-core}).
    file_imports
        .get(leading)
        .is_some_and(|modules| modules.contains(&normalize_module_key(leading)))
}

fn unique_module_for(modules: &HashSet<String>) -> Option<String> {
    (modules.len() == 1)
        .then(|| modules.iter().next().cloned())
        .flatten()
}

fn single<T>(items: &[T]) -> Option<&T> {
    (items.len() == 1).then(|| &items[0])
}

pub fn merge(results: Vec<ExtractionResult>) -> Graph {
    let mut graph = Graph::new();

    let mut file_imports: HashMap<String, HashSet<String>> = HashMap::new();
    for result in &results {
        for import in &result.imports {
            if let Some(first_node) = result.nodes.first() {
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

    for result in &results {
        graph.nodes.extend(result.nodes.iter().cloned());
    }

    let node_ids: HashSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();

    let mut name_to_entries: HashMap<&str, Vec<NameEntry>> = HashMap::new();
    for node in &graph.nodes {
        if !is_resolvable_symbol_kind(node.kind) {
            continue;
        }
        name_to_entries
            .entry(node.name.as_str())
            .or_default()
            .push(NameEntry {
                id: node.id.clone(),
                module: node.module.clone(),
                file: node.file.to_string_lossy().to_string(),
            });
    }

    let id_to_info: HashMap<&str, (Option<&str>, &str)> = graph
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                (node.module.as_deref(), node.file.to_str().unwrap_or("")),
            )
        })
        .collect();

    let id_to_name: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();
    let candidate_file_stems: HashMap<String, String> = graph
        .nodes
        .iter()
        .filter_map(|node| {
            node.file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| (node.id.clone(), stem.to_ascii_lowercase()))
        })
        .collect();
    let mut candidate_to_owner_names: HashMap<String, Vec<String>> = HashMap::new();
    for result in &results {
        for edge in &result.edges {
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

    // Cross-boundary resolution indexes: per-file imports + canonical defs.
    let imported_symbol_modules = build_imported_symbol_modules(&results);
    let module_symbol_index = build_module_symbol_index(graph.nodes.iter().map(|node| {
        (
            node.id.as_str(),
            node.kind,
            node.module.as_deref(),
            node.name.as_str(),
        )
    }));
    let id_to_kind: HashMap<&str, NodeKind> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.kind))
        .collect();

    let all_edges: Vec<_> = results
        .into_iter()
        .flat_map(|result| result.edges)
        .collect();

    // Build child → parent type mapping for scoping Reads edges.
    // If source X is contained by type T, reads from X should prefer
    // targets that are also contained by T (siblings in the same type).
    let mut child_to_parents: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &all_edges {
        if edge.kind == EdgeKind::Contains
            && node_ids.contains(edge.target.as_str())
            && node_ids.contains(edge.source.as_str())
        {
            child_to_parents
                .entry(edge.target.clone())
                .or_default()
                .push(edge.source.clone());
        }
    }

    for mut edge in all_edges {
        let is_external_usr_call = edge.target.starts_with("s:")
            && edge.kind == EdgeKind::Calls
            && !node_ids.contains(edge.target.as_str());

        if node_ids.contains(edge.target.as_str())
            || edge.kind == EdgeKind::Uses
            || (edge.kind == EdgeKind::Calls
                && (edge.direction.is_some() || edge.operation.is_some()))
            || is_external_usr_call
        {
            graph.edges.push(edge);
            continue;
        }

        let (source_module, source_file) = id_to_info
            .get(edge.source.as_str())
            .copied()
            .unwrap_or((None, ""));

        // Implements edges whose trait target dangles (the trait lives in
        // another file/crate): bind to the canonical trait node via imports,
        // else via a unique same-module trait of that name. If neither pins it
        // (external traits like `Default`/`From`), keep the edge as written so
        // the relationship is still recorded.
        if edge.kind == EdgeKind::Implements {
            let bind_ctx = ImportBindContext {
                source_file,
                imported_symbol_modules: &imported_symbol_modules,
                module_symbol_index: &module_symbol_index,
                name_to_entries: &name_to_entries,
                candidate_to_owner_names: &candidate_to_owner_names,
                id_to_kind: &id_to_kind,
            };
            if let Some(bound) = resolve_implements_target(&edge.target, source_module, &bind_ctx) {
                if bound != edge.target {
                    edge.confidence *= IMPORT_BOUND_CONFIDENCE;
                }
                edge.target = bound;
            }
            graph.edges.push(edge);
            continue;
        }

        // Cross-boundary calls / type refs: when the target can't be resolved
        // by same-name local resolution, try import-guided binding first.
        if matches!(edge.kind, EdgeKind::Calls | EdgeKind::TypeRef) {
            let bind_ctx = ImportBindContext {
                source_file,
                imported_symbol_modules: &imported_symbol_modules,
                module_symbol_index: &module_symbol_index,
                name_to_entries: &name_to_entries,
                candidate_to_owner_names: &candidate_to_owner_names,
                id_to_kind: &id_to_kind,
            };
            if let Some(bound) = resolve_via_imports(&edge.target, edge.kind, &bind_ctx) {
                edge.target = bound;
                edge.confidence *= IMPORT_BOUND_CONFIDENCE;
                graph.edges.push(edge);
                continue;
            }
        }

        let target_name = target_symbol_name(&edge.target);
        let Some(candidates) = name_to_entries.get(target_name) else {
            continue;
        };
        if candidates.is_empty() {
            continue;
        }

        let source_imports = file_imports.get(source_file);
        let source_owner_names = candidate_to_owner_names
            .get(&edge.source)
            .cloned()
            .unwrap_or_default();
        let owned_hint = target_prefix_hint(&edge.target);
        let prefix_hint = edge.operation.as_deref().or(owned_hint.as_deref());

        if candidates.len() == 1 {
            let candidate = &candidates[0];
            let same_module = modules_match(source_module, candidate.module.as_deref());
            if same_module {
                if let Some(hint) = prefix_hint
                    && !candidate_matches_hint(
                        candidate,
                        hint,
                        &source_owner_names,
                        &candidate_to_owner_names,
                        &candidate_file_stems,
                    )
                    && should_enforce_hint(&edge.target, hint)
                {
                    continue;
                }
                edge.target = candidate.id.clone();
                edge.confidence *= 0.9;
                graph.edges.push(edge);
            } else {
                let imported = source_imports
                    .and_then(|imports| {
                        candidate
                            .module
                            .as_deref()
                            .map(|module| imports.contains(module))
                    })
                    .unwrap_or(false);
                if imported {
                    edge.target = candidate.id.clone();
                    edge.confidence *= 0.7;
                    graph.edges.push(edge);
                }
            }
            continue;
        }

        // For Reads edges: scope resolution to siblings of the same type.
        // Without this, "viewModel" resolves to ALL viewModel properties in the module.
        //
        // Strategy: use USR prefix matching. If source is s:4Room0A4PageV4bodyQrvp,
        // its type prefix is s:4Room0A4PageV. Prefer candidates whose ID shares
        // this prefix (they're members of the same type). Falls back to Contains
        // edge lookup, then same-file, then normal resolution.
        if edge.kind == EdgeKind::Reads && candidates.len() > 1 {
            // Try USR prefix: strip the member suffix to get the type prefix
            let usr_prefix = if edge.source.starts_with("s:") {
                usr_type_prefix(&edge.source)
            } else {
                None
            };

            if let Some(prefix) = usr_prefix {
                let siblings: Vec<&NameEntry> = candidates
                    .iter()
                    .filter(|c| c.id.starts_with(&prefix))
                    .collect();
                if siblings.len() == 1 {
                    edge.target = siblings[0].id.clone();
                    edge.confidence *= 0.9;
                    graph.edges.push(edge);
                    continue;
                }
                if !siblings.is_empty() {
                    // Multiple siblings with same prefix — pick same file
                    let same_file: Vec<&&NameEntry> = siblings
                        .iter()
                        .filter(|c| {
                            id_to_info
                                .get(c.id.as_str())
                                .is_some_and(|(_, f)| *f == source_file)
                        })
                        .collect();
                    if same_file.len() == 1 {
                        edge.target = same_file[0].id.clone();
                        edge.confidence *= 0.9;
                        graph.edges.push(edge);
                        continue;
                    }
                }
                // No siblings found with same USR prefix — this property
                // is not a member of the source's type. Drop the read edge
                // rather than resolving to unrelated types.
                continue;
            }

            // Fallback: Contains-edge-based sibling matching
            if let Some(source_owners) = child_to_parents.get(&edge.source) {
                let sibling_candidates: Vec<&NameEntry> = candidates
                    .iter()
                    .filter(|c| {
                        candidate_to_owner_names.get(&c.id).is_some_and(|owners| {
                            owners.iter().any(|owner| {
                                source_owners.iter().any(|so| {
                                    id_to_name.get(so.as_str()).is_some_and(|n| *n == owner)
                                })
                            })
                        })
                    })
                    .collect();
                if sibling_candidates.len() == 1 {
                    edge.target = sibling_candidates[0].id.clone();
                    edge.confidence *= 0.9;
                    graph.edges.push(edge);
                    continue;
                }
            }
        }

        let resolve_context = ResolveContext {
            raw_target: &edge.target,
            source_module,
            source_imports,
            prefix_hint,
            source_owner_names: &source_owner_names,
            candidate_to_owner_names: &candidate_to_owner_names,
            candidate_file_stems: &candidate_file_stems,
        };
        let resolved = resolve_candidates(candidates, &resolve_context);
        for (candidate_id, factor) in resolved {
            let mut resolved_edge = edge.clone();
            resolved_edge.target = candidate_id;
            resolved_edge.confidence *= factor;
            graph.edges.push(resolved_edge);
        }
    }

    graph
}

/// Extract the type prefix from a USR string.
/// e.g., "s:4Room0A4PageV4bodyQrvp" → "s:4Room0A4PageV"
/// USR structure: s:<module><type>V<member> where V marks the type boundary.
fn usr_type_prefix(usr: &str) -> Option<String> {
    // Find the last 'V' that's followed by lowercase (member name start)
    // Swift USRs use V to end type names: s:4Room0A4PageV4bodyQrvp
    //                                                    ^ type ends here
    let bytes = usr.as_bytes();
    let mut last_v_pos = None;
    for i in (2..bytes.len()).rev() {
        if bytes[i] == b'V'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_digit() || bytes[i + 1].is_ascii_lowercase())
        {
            last_v_pos = Some(i + 1);
            break;
        }
    }
    last_v_pos.map(|pos| usr[..pos].to_string())
}

fn candidate_matches_hint(
    candidate: &NameEntry,
    hint: &str,
    source_owner_names: &[String],
    candidate_to_owner_names: &HashMap<String, Vec<String>>,
    candidate_file_stems: &HashMap<String, String>,
) -> bool {
    let normalized_hint = hint.to_ascii_lowercase();

    if matches!(normalized_hint.as_str(), "self" | "this") {
        return source_owner_names.iter().any(|source_owner| {
            candidate_to_owner_names
                .get(&candidate.id)
                .is_some_and(|owners| {
                    owners
                        .iter()
                        .any(|owner| owner.eq_ignore_ascii_case(source_owner))
                })
        });
    }

    if candidate_to_owner_names
        .get(&candidate.id)
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner.eq_ignore_ascii_case(hint)
                    || owner
                        .to_ascii_lowercase()
                        .starts_with(normalized_hint.as_str())
            })
        })
    {
        return true;
    }

    candidate_file_stems
        .get(&candidate.id)
        .is_some_and(|stem| stem == &normalized_hint)
}

fn resolve_candidates(
    candidates: &[NameEntry],
    context: &ResolveContext<'_>,
) -> Vec<(String, f64)> {
    let same_module: Vec<&NameEntry> = candidates
        .iter()
        .filter(|candidate| modules_match(context.source_module, candidate.module.as_deref()))
        .collect();
    if same_module.len() == 1 {
        if let Some(hint) = context.prefix_hint
            && !candidate_matches_hint(
                same_module[0],
                hint,
                context.source_owner_names,
                context.candidate_to_owner_names,
                context.candidate_file_stems,
            )
            && should_enforce_hint(context.raw_target, hint)
        {
            return Vec::new();
        }
        return vec![(same_module[0].id.clone(), 0.9)];
    }

    if same_module.len() > 1 {
        if let Some(hint) = context.prefix_hint {
            let narrowed: Vec<&&NameEntry> = same_module
                .iter()
                .filter(|candidate| {
                    candidate_matches_hint(
                        candidate,
                        hint,
                        context.source_owner_names,
                        context.candidate_to_owner_names,
                        context.candidate_file_stems,
                    )
                })
                .collect();
            if narrowed.len() == 1 {
                return vec![(narrowed[0].id.clone(), 0.85)];
            }
            if narrowed.is_empty() && should_enforce_hint(context.raw_target, hint) {
                return Vec::new();
            }
        }

        // Cap ambiguous resolution: if too many distinct files contain
        // candidates after disambiguation, drop the edge entirely. A missing
        // edge is better than N false positives (e.g., "horizontal", "top").
        // Count unique files, not raw candidates, so a type with extensions
        // in the same file isn't penalized.
        let unique_files: HashSet<&str> = same_module.iter().map(|c| c.file.as_str()).collect();
        if unique_files.len() > 3 {
            return Vec::new();
        }
        return same_module
            .iter()
            .map(|candidate| (candidate.id.clone(), 0.4))
            .collect();
    }

    if let Some(imports) = context.source_imports {
        let imported: Vec<&NameEntry> = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .module
                    .as_deref()
                    .is_some_and(|module| imports.contains(module))
            })
            .collect();
        if imported.len() == 1 {
            return vec![(imported[0].id.clone(), 0.8)];
        }
        if imported.len() > 1 {
            let unique_files: HashSet<&str> = imported.iter().map(|c| c.file.as_str()).collect();
            if unique_files.len() > 3 {
                return Vec::new();
            }
            return imported
                .iter()
                .map(|candidate| (candidate.id.clone(), 0.3))
                .collect();
        }
    }

    Vec::new()
}

fn modules_match(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
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
        let left = ExtractionResult {
            nodes: vec![make_node("a::Foo", "Foo", NodeKind::Struct)],
            edges: vec![],
            imports: vec![],
        };
        let right = ExtractionResult {
            nodes: vec![make_node("b::Bar", "Bar", NodeKind::Struct)],
            edges: vec![],
            imports: vec![],
        };

        let graph = merge(vec![left, right]);
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn drops_edges_with_unresolved_targets() {
        let result = ExtractionResult {
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

        let graph = merge(vec![result]);
        assert_eq!(graph.edges.len(), 0);
    }

    #[test]
    fn keeps_uses_edges_even_if_target_unresolved() {
        let result = ExtractionResult {
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

        let graph = merge(vec![result]);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn same_module_candidate_wins() {
        let mut helper = make_node("mod_a::helper", "helper", NodeKind::Function);
        helper.module = Some("mod_a".to_string());
        let mut caller = make_node("mod_a::main", "main", NodeKind::Function);
        caller.module = Some("mod_a".to_string());

        let graph = merge(vec![
            ExtractionResult {
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
            },
            ExtractionResult {
                nodes: vec![helper],
                edges: vec![],
                imports: vec![],
            },
        ]);

        let call_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Calls)
            .unwrap();
        assert_eq!(call_edge.target, "mod_a::helper");
        assert!((call_edge.confidence - 0.9).abs() < 0.001);
    }

    #[test]
    fn owner_hint_disambiguates_same_module_candidates() {
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
        let graph = merge(vec![
            ExtractionResult {
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
            },
            ExtractionResult {
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
            },
        ]);

        let type_refs: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == source && edge.kind == EdgeKind::TypeRef)
            .collect();

        assert_eq!(type_refs.len(), 1);
        assert_eq!(type_refs[0].target, "room::RoomPage::chatRoomFragViewPanel");
        assert!((type_refs[0].confidence - 0.85).abs() < 0.001);
    }

    #[test]
    fn qualified_call_hint_drops_false_local_resolution() {
        let caller = make_node("sqlite.rs::open", "open", NodeKind::Function);
        let callee = make_node("sqlite.rs::helper", "open", NodeKind::Function);

        let graph = merge(vec![ExtractionResult {
            nodes: vec![caller, callee],
            edges: vec![Edge {
                source: "sqlite.rs::open".to_string(),
                target: "Connection::open".to_string(),
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
        }]);

        assert!(graph.edges.is_empty());
    }

    #[test]
    fn module_hint_resolves_call_by_file_stem() {
        let caller = Node {
            file: PathBuf::from("lib.rs"),
            ..make_node("lib.rs::run", "run", NodeKind::Function)
        };
        let callee = Node {
            file: PathBuf::from("utils.rs"),
            ..make_node("utils.rs::helper", "helper", NodeKind::Function)
        };

        let graph = merge(vec![ExtractionResult {
            nodes: vec![caller, callee],
            edges: vec![Edge {
                source: "lib.rs::run".to_string(),
                target: "utils::helper".to_string(),
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
        }]);

        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].target, "utils.rs::helper");
    }

    #[test]
    fn self_field_read_resolves_to_matching_field() {
        let struct_node = make_node("sqlite.rs::SqliteStore", "SqliteStore", NodeKind::Struct);
        let field_node = make_node("sqlite.rs::SqliteStore.path", "path", NodeKind::Field);
        let impl_node = make_node("sqlite.rs::impl_SqliteStore", "SqliteStore", NodeKind::Impl);
        let fn_node = make_node(
            "sqlite.rs::impl_SqliteStore::open",
            "open",
            NodeKind::Function,
        );

        let graph = merge(vec![ExtractionResult {
            nodes: vec![struct_node, field_node, impl_node, fn_node],
            edges: vec![
                Edge {
                    source: "sqlite.rs::SqliteStore".to_string(),
                    target: "sqlite.rs::SqliteStore.path".to_string(),
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
                    source: "sqlite.rs::impl_SqliteStore".to_string(),
                    target: "sqlite.rs::impl_SqliteStore::open".to_string(),
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
                    source: "sqlite.rs::impl_SqliteStore::open".to_string(),
                    target: "self.path".to_string(),
                    kind: EdgeKind::Reads,
                    confidence: 0.8,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
            imports: vec![],
        }]);

        let read_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Reads)
            .unwrap();
        assert_eq!(read_edge.target, "sqlite.rs::SqliteStore.path");
    }

    // ---- Cross-boundary (import-guided) resolution ----------------------

    fn node_in(id: &str, name: &str, kind: NodeKind, module: &str, file: &str) -> Node {
        Node {
            module: Some(module.to_string()),
            file: PathBuf::from(file),
            ..make_node(id, name, kind)
        }
    }

    fn edge_of(source: &str, target: &str, kind: EdgeKind) -> Edge {
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

    fn named_import(path: &str, symbols: &[&str]) -> crate::resolve::Import {
        crate::resolve::Import {
            path: path.to_string(),
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
            kind: crate::resolve::ImportKind::Named,
        }
    }

    #[test]
    fn cross_crate_type_ref_resolves_through_import() {
        // Caller crate does `use other::Thing;` then `field: Thing`.
        let def = node_in(
            "lib_a/src/lib.rs::Thing",
            "Thing",
            NodeKind::Struct,
            "other",
            "lib_a/src/lib.rs",
        );
        let caller_field = node_in(
            "lib_b/src/use.rs::Holder.value",
            "value",
            NodeKind::Field,
            "consumer",
            "lib_b/src/use.rs",
        );

        let graph = merge(vec![
            ExtractionResult {
                nodes: vec![def],
                edges: vec![],
                imports: vec![],
            },
            ExtractionResult {
                nodes: vec![caller_field],
                // Bare type ref `Thing` qualified file-locally by the extractor.
                edges: vec![edge_of(
                    "lib_b/src/use.rs::Holder.value",
                    "lib_b/src/use.rs::Thing",
                    EdgeKind::TypeRef,
                )],
                imports: vec![named_import("other", &["Thing"])],
            },
        ]);

        let type_ref = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::TypeRef)
            .expect("type ref should survive");
        assert_eq!(type_ref.target, "lib_a/src/lib.rs::Thing");
        assert!((type_ref.confidence - IMPORT_BOUND_CONFIDENCE).abs() < 1e-9);
    }

    #[test]
    fn cross_crate_associated_call_resolves_to_method() {
        // `use other::Thing; Thing::new()` must land on other::Thing::new,
        // not on some same-named `new` in the caller crate.
        let thing = node_in(
            "lib_a/src/lib.rs::Thing",
            "Thing",
            NodeKind::Struct,
            "other",
            "lib_a/src/lib.rs",
        );
        let thing_new = node_in(
            "lib_a/src/lib.rs::impl_Thing::new",
            "new",
            NodeKind::Function,
            "other",
            "lib_a/src/lib.rs",
        );
        let thing_impl = node_in(
            "lib_a/src/lib.rs::impl_Thing",
            "Thing",
            NodeKind::Impl,
            "other",
            "lib_a/src/lib.rs",
        );
        // A decoy `new` in the consumer crate that must NOT be chosen.
        let decoy_new = node_in(
            "lib_b/src/use.rs::impl_Other::new",
            "new",
            NodeKind::Function,
            "consumer",
            "lib_b/src/use.rs",
        );
        let decoy_impl = node_in(
            "lib_b/src/use.rs::impl_Other",
            "Other",
            NodeKind::Impl,
            "consumer",
            "lib_b/src/use.rs",
        );
        let caller = node_in(
            "lib_b/src/use.rs::run",
            "run",
            NodeKind::Function,
            "consumer",
            "lib_b/src/use.rs",
        );

        let graph = merge(vec![
            ExtractionResult {
                nodes: vec![thing, thing_impl, thing_new],
                edges: vec![edge_of(
                    "lib_a/src/lib.rs::impl_Thing",
                    "lib_a/src/lib.rs::impl_Thing::new",
                    EdgeKind::Contains,
                )],
                imports: vec![],
            },
            ExtractionResult {
                nodes: vec![caller, decoy_impl, decoy_new],
                edges: vec![
                    edge_of(
                        "lib_b/src/use.rs::impl_Other",
                        "lib_b/src/use.rs::impl_Other::new",
                        EdgeKind::Contains,
                    ),
                    edge_of("lib_b/src/use.rs::run", "Thing::new", EdgeKind::Calls),
                ],
                imports: vec![named_import("other", &["Thing"])],
            },
        ]);

        let call = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Calls)
            .expect("cross-crate call should resolve");
        assert_eq!(call.target, "lib_a/src/lib.rs::impl_Thing::new");
        assert!((call.confidence - IMPORT_BOUND_CONFIDENCE).abs() < 1e-9);
    }

    #[test]
    fn trait_impl_in_other_file_binds_to_canonical_trait() {
        // Trait declared in file A, implemented in file B (same crate, cross
        // file). The dangling `<fileB>::Drawable` target must rebind to the
        // real trait node.
        let trait_node = node_in(
            "shapes/src/draw.rs::Drawable",
            "Drawable",
            NodeKind::Trait,
            "shapes",
            "shapes/src/draw.rs",
        );
        let circle = node_in(
            "shapes/src/circle.rs::Circle",
            "Circle",
            NodeKind::Struct,
            "shapes",
            "shapes/src/circle.rs",
        );

        let graph = merge(vec![
            ExtractionResult {
                nodes: vec![trait_node],
                edges: vec![],
                imports: vec![],
            },
            ExtractionResult {
                nodes: vec![circle],
                // `impl Drawable for Circle` → implements edge with a
                // file-local phantom trait target.
                edges: vec![edge_of(
                    "shapes/src/circle.rs::Circle",
                    "shapes/src/circle.rs::Drawable",
                    EdgeKind::Implements,
                )],
                // same-crate import via `super` carries no crate alias; the
                // same-module trait fallback must still bind it.
                imports: vec![],
            },
        ]);

        let implements = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Implements)
            .expect("implements edge should be kept");
        assert_eq!(implements.target, "shapes/src/draw.rs::Drawable");
    }

    #[test]
    fn cross_crate_trait_impl_binds_through_import() {
        let trait_node = node_in(
            "lib_a/src/lib.rs::Storage",
            "Storage",
            NodeKind::Trait,
            "other",
            "lib_a/src/lib.rs",
        );
        let impl_type = node_in(
            "lib_b/src/mem.rs::MemStore",
            "MemStore",
            NodeKind::Struct,
            "consumer",
            "lib_b/src/mem.rs",
        );

        let graph = merge(vec![
            ExtractionResult {
                nodes: vec![trait_node],
                edges: vec![],
                imports: vec![],
            },
            ExtractionResult {
                nodes: vec![impl_type],
                edges: vec![edge_of(
                    "lib_b/src/mem.rs::MemStore",
                    "lib_b/src/mem.rs::Storage",
                    EdgeKind::Implements,
                )],
                imports: vec![named_import("other", &["Storage"])],
            },
        ]);

        let implements = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Implements)
            .expect("implements edge should be kept");
        assert_eq!(implements.target, "lib_a/src/lib.rs::Storage");
    }

    #[test]
    fn external_trait_impl_kept_unbound() {
        // `impl Default for Config` — `Default` is external (std), not in the
        // graph and not imported. The edge must survive unchanged.
        let config = node_in(
            "lib_a/src/lib.rs::Config",
            "Config",
            NodeKind::Struct,
            "other",
            "lib_a/src/lib.rs",
        );

        let graph = merge(vec![ExtractionResult {
            nodes: vec![config],
            edges: vec![edge_of(
                "lib_a/src/lib.rs::Config",
                "lib_a/src/lib.rs::Default",
                EdgeKind::Implements,
            )],
            imports: vec![],
        }]);

        let implements = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Implements)
            .expect("external implements edge should be kept");
        assert_eq!(implements.target, "lib_a/src/lib.rs::Default");
        assert!((implements.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn unimported_type_ref_is_not_bound_to_same_name() {
        // No import maps `Thing` here, so the cross-crate `Thing` def must NOT
        // be picked up — guards against same-name false edges.
        let def = node_in(
            "lib_a/src/lib.rs::Thing",
            "Thing",
            NodeKind::Struct,
            "other",
            "lib_a/src/lib.rs",
        );
        let caller_field = node_in(
            "lib_b/src/use.rs::Holder.value",
            "value",
            NodeKind::Field,
            "consumer",
            "lib_b/src/use.rs",
        );

        let graph = merge(vec![
            ExtractionResult {
                nodes: vec![def],
                edges: vec![],
                imports: vec![],
            },
            ExtractionResult {
                nodes: vec![caller_field],
                edges: vec![edge_of(
                    "lib_b/src/use.rs::Holder.value",
                    "lib_b/src/use.rs::Thing",
                    EdgeKind::TypeRef,
                )],
                imports: vec![],
            },
        ]);

        assert!(
            graph
                .edges
                .iter()
                .all(|edge| edge.kind != EdgeKind::TypeRef),
            "unimported same-name type ref must be dropped, not bound"
        );
    }

    #[test]
    fn rust_crate_name_underscore_matches_hyphen_module() {
        // `use nous_core::Confidence` (underscore) must resolve to a node whose
        // module is `nous-core` (hyphen).
        let def = node_in(
            "crates/nous-core/src/confidence.rs::Confidence",
            "Confidence",
            NodeKind::Struct,
            "nous-core",
            "crates/nous-core/src/confidence.rs",
        );
        let field = node_in(
            "crates/nous-arxiv/src/staged.rs::Staged.confidence",
            "confidence",
            NodeKind::Field,
            "nous-arxiv",
            "crates/nous-arxiv/src/staged.rs",
        );

        let graph = merge(vec![
            ExtractionResult {
                nodes: vec![def],
                edges: vec![],
                imports: vec![],
            },
            ExtractionResult {
                nodes: vec![field],
                edges: vec![edge_of(
                    "crates/nous-arxiv/src/staged.rs::Staged.confidence",
                    "crates/nous-arxiv/src/staged.rs::Confidence",
                    EdgeKind::TypeRef,
                )],
                imports: vec![named_import("nous_core", &["Confidence", "PackId"])],
            },
        ]);

        let type_ref = graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::TypeRef)
            .expect("type ref should resolve across underscore/hyphen");
        assert_eq!(
            type_ref.target,
            "crates/nous-core/src/confidence.rs::Confidence"
        );
    }
}
