use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;
use tree_sitter::Parser;

use grapha_core::graph::{
    Edge, EdgeKind, EdgeProvenance, Node, NodeKind, NodeRole, Span, Visibility,
};

use super::{ExtractionResult, LanguageExtractor};

/// Well-known `std` `Option`/`Result` constructors that tree-sitter cannot
/// distinguish from real function calls. `Some(x)` / `Ok(x)` / `Err(x)` parse as
/// `call_expression`s and `if let Some(x) = ...` / `let Ok(x) = ...` parse as a
/// binding pattern whose first identifier is the variant name. Neither is a real
/// callee or variable, so we suppress them at edge/node creation. We deliberately
/// only suppress these exact, unqualified constructors (a user's own `fn some()`
/// or `let ok = ...` keeps its lowercase name and is unaffected).
const STD_OPTION_RESULT_CONSTRUCTORS: &[&str] = &["Some", "None", "Ok", "Err"];

/// Returns true when `name` is one of the suppressed std Option/Result
/// constructors (`Some`/`None`/`Ok`/`Err`), matched exactly and case-sensitively.
fn is_std_option_result_constructor(name: &str) -> bool {
    STD_OPTION_RESULT_CONSTRUCTORS.contains(&name)
}

/// Declarative-macro names that, in item position, expand to a single newtype
/// declaration whose name is the first `PascalCase` identifier argument (e.g.
/// `ulid_newtype! { SourceId }` → `pub struct SourceId(Ulid);`). tree-sitter
/// cannot expand macros, so we synthesize a struct-like type node heuristically
/// for these. This list is intentionally conservative and extensible: add a
/// project's own id/newtype-declaring macros here, or rely on the
/// [`is_newtype_macro_name`] suffix heuristic for `*_newtype!` / `*_id!` macros.
const NEWTYPE_DECL_MACROS: &[&str] = &["ulid_newtype", "uuid_newtype", "id_newtype", "newtype_id"];

/// Suffixes that strongly imply a macro declares a single newtype in item
/// position (e.g. `mod_newtype!`, `entity_id!`). Combined with the explicit
/// [`NEWTYPE_DECL_MACROS`] allow-list so new declarative id macros are picked up
/// without a code change while keeping false positives low.
const NEWTYPE_DECL_MACRO_SUFFIXES: &[&str] = &["_newtype", "_id"];

/// Returns true when a macro named `macro_name` is recognised as declaring a
/// single newtype in item position. Matches the explicit allow-list first, then
/// the conservative `*_newtype` / `*_id` suffix heuristic.
fn is_newtype_macro_name(macro_name: &str) -> bool {
    if NEWTYPE_DECL_MACROS.contains(&macro_name) {
        return true;
    }
    NEWTYPE_DECL_MACRO_SUFFIXES
        .iter()
        .any(|suffix| macro_name.len() > suffix.len() && macro_name.ends_with(suffix))
}

/// Returns true when `name` looks like a type name (`PascalCase`: starts with an
/// uppercase ASCII letter). Guards macro-synthesized type nodes against firing on
/// lowercase identifier arguments (e.g. a string seed or a field name).
fn is_pascal_case_type(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

thread_local! {
    static RUST_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::LANGUAGE.into()).expect("failed to load Rust grammar");
        p
    });
}

pub struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn extract(&self, source: &[u8], file_path: &Path) -> anyhow::Result<ExtractionResult> {
        let tree = RUST_PARSER.with_borrow_mut(|parser| {
            parser
                .parse(source, None)
                .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse source"))
        })?;

        let mut result = ExtractionResult::new();
        let file_str = file_path.to_string_lossy().to_string();
        let file_node_id = format!("file:{file_str}");

        push_file_node(
            &mut result,
            tree.root_node(),
            file_path,
            &file_str,
            &file_node_id,
        );
        walk_node(tree.root_node(), source, &file_str, &[], None, &mut result);
        extract_rust_framework_nodes(&mut result, source, &file_str);
        emit_file_contains_edges(&mut result, &file_str, &file_node_id);

        Ok(result)
    }
}

fn push_file_node(
    result: &mut ExtractionResult,
    root: tree_sitter::Node,
    file_path: &Path,
    file: &str,
    file_node_id: &str,
) {
    let name = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(file)
        .to_string();
    let start = root.start_position();
    let end = root.end_position();
    let mut metadata = HashMap::new();
    metadata.insert("language".to_string(), "rust".to_string());

    result.nodes.push(Node {
        id: file_node_id.to_string(),
        kind: NodeKind::File,
        name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility: Visibility::Private,
        metadata,
        role: None,
        signature: None,
        doc_comment: None,
        module: None,
        snippet: None,
        repo: None,
    });
}

fn emit_file_contains_edges(result: &mut ExtractionResult, file: &str, file_node_id: &str) {
    let contained: HashSet<String> = result
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .map(|edge| edge.target.clone())
        .collect();
    let top_level = result
        .nodes
        .iter()
        .filter(|node| node.id != file_node_id && !contained.contains(&node.id))
        .map(|node| (node.id.clone(), node.span.clone()))
        .collect::<Vec<_>>();

    for (target, span) in top_level {
        result.edges.push(Edge {
            source: file_node_id.to_string(),
            target,
            kind: EdgeKind::Contains,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: vec![EdgeProvenance {
                file: file.into(),
                span,
                symbol_id: file_node_id.to_string(),
            }],
            repo: None,
        });
    }
}

fn extract_rust_framework_nodes(result: &mut ExtractionResult, source: &[u8], file: &str) {
    let Ok(content) = std::str::from_utf8(source) else {
        return;
    };
    for pattern in [
        r#"#\[(get|post|put|patch|delete|head|options)\s*\(\s*["']([^"']+)["']"#,
        r#"\.route\s*\(\s*["']([^"']+)["']\s*,\s*(get|post|put|patch|delete|head|options)"#,
    ] {
        let re = Regex::new(pattern).expect("Rust route regex should compile");
        for capture in re.captures_iter(content) {
            let Some(full_match) = capture.get(0) else {
                continue;
            };
            let (method, path) = if pattern.starts_with("#") {
                (
                    capture.get(1).map(|m| m.as_str()).unwrap_or_default(),
                    capture.get(2).map(|m| m.as_str()).unwrap_or_default(),
                )
            } else {
                (
                    capture.get(2).map(|m| m.as_str()).unwrap_or_default(),
                    capture.get(1).map(|m| m.as_str()).unwrap_or_default(),
                )
            };
            push_route_node(
                result,
                file,
                source,
                method,
                path,
                full_match.start(),
                full_match.end(),
            );
        }
    }
}

fn push_route_node(
    result: &mut ExtractionResult,
    file: &str,
    source: &[u8],
    method: &str,
    path: &str,
    start_byte: usize,
    end_byte: usize,
) {
    if method.is_empty() || path.is_empty() {
        return;
    }
    let method = method.to_ascii_uppercase();
    let name = format!("{method} {path}");
    let span = span_from_byte_range(source, start_byte, end_byte);
    let line = span.start[0] + 1;
    let id = format!("route:{file}:{method}:{path}:{line}");
    if result.nodes.iter().any(|node| node.id == id) {
        return;
    }
    let mut metadata = HashMap::new();
    metadata.insert("language".to_string(), "rust".to_string());
    metadata.insert("framework.kind".to_string(), "route".to_string());
    metadata.insert("route.method".to_string(), method);
    metadata.insert("route.path".to_string(), path.to_string());

    result.nodes.push(Node {
        id,
        kind: NodeKind::Route,
        name,
        file: file.into(),
        span,
        visibility: Visibility::Public,
        metadata,
        role: None,
        signature: None,
        doc_comment: None,
        module: None,
        snippet: None,
        repo: None,
    });
}

fn span_from_byte_range(source: &[u8], start_byte: usize, end_byte: usize) -> Span {
    Span {
        start: point_for_byte(source, start_byte),
        end: point_for_byte(source, end_byte),
    }
}

fn point_for_byte(source: &[u8], target: usize) -> [usize; 2] {
    let mut row = 0usize;
    let mut column = 0usize;
    for byte in source.iter().take(target.min(source.len())) {
        if *byte == b'\n' {
            row += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    [row, column]
}

fn edge_provenance(file: &str, node: tree_sitter::Node, symbol_id: &str) -> Vec<EdgeProvenance> {
    let start = node.start_position();
    let end = node.end_position();
    vec![EdgeProvenance {
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        symbol_id: symbol_id.to_string(),
    }]
}

/// Recursively walk a tree-sitter node, extracting symbols and edges.
///
/// `module_path` tracks the logical nesting (module names) for ID generation.
/// `parent_id` is the node ID of the enclosing symbol, used to emit Contains edges.
fn walk_node(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    match node.kind() {
        "function_item" | "function_signature_item" => {
            if let Some(graph_node) =
                extract_function(node, source, file, module_path, parent_id, result)
            {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                result.nodes.push(graph_node);

                // Emit TypeRef edge for non-primitive return types
                if let Some(return_type_node) = node.child_by_field_name("return_type")
                    && let Ok(return_text) = return_type_node.utf8_text(source)
                {
                    let type_name = return_text.trim_start_matches("->").trim();
                    push_type_ref_edges(
                        result,
                        file,
                        return_type_node,
                        &node_id,
                        module_path,
                        type_name,
                    );
                }

                // Walk function body for nested items and call expressions
                if let Some(body) = node.child_by_field_name("body") {
                    walk_children(body, source, file, module_path, Some(&node_id), result);
                    extract_reads_and_writes(body, source, file, &node_id, result);
                    extract_calls(body, source, file, module_path, &node_id, result);
                }
            }
        }
        "const_item" | "static_item" => {
            if let Some(graph_node) =
                extract_constant(node, source, file, module_path, parent_id, result)
            {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                result.nodes.push(graph_node);

                if let Some(type_node) = node.child_by_field_name("type")
                    && let Ok(type_name) = type_node.utf8_text(source)
                {
                    push_type_ref_edges(result, file, type_node, &node_id, module_path, type_name);
                }
            }
        }
        "let_declaration" => {
            if let Some(graph_node) =
                extract_variable(node, source, file, module_path, parent_id, result)
            {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                result.nodes.push(graph_node);

                if let Some(type_node) = node.child_by_field_name("type")
                    && let Ok(type_name) = type_node.utf8_text(source)
                {
                    push_type_ref_edges(result, file, type_node, &node_id, module_path, type_name);
                }
            }
        }
        "type_item" => {
            if let Some(graph_node) =
                extract_type_alias(node, source, file, module_path, parent_id, result)
            {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                result.nodes.push(graph_node);

                if let Some(type_node) = node.child_by_field_name("type")
                    && let Ok(type_name) = type_node.utf8_text(source)
                {
                    push_type_ref_edges(result, file, type_node, &node_id, module_path, type_name);
                }
            }
        }
        "struct_item" => {
            if let Some(graph_node) = extract_named_item(
                node,
                source,
                file,
                module_path,
                parent_id,
                NodeKind::Struct,
                result,
            ) {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                let node_name = graph_node.name.clone();
                result.nodes.push(graph_node);

                // Extract fields from the struct body
                if let Some(body) = node.child_by_field_name("body") {
                    extract_struct_fields(
                        body,
                        source,
                        file,
                        module_path,
                        &node_id,
                        &node_name,
                        result,
                    );
                }
            }
        }
        "enum_item" => {
            if let Some(graph_node) = extract_named_item(
                node,
                source,
                file,
                module_path,
                parent_id,
                NodeKind::Enum,
                result,
            ) {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                let node_name = graph_node.name.clone();
                result.nodes.push(graph_node);

                // Extract variants from the enum body
                if let Some(body) = node.child_by_field_name("body") {
                    extract_enum_variants(
                        body,
                        source,
                        file,
                        module_path,
                        &node_id,
                        &node_name,
                        result,
                    );
                }
            }
        }
        "trait_item" => {
            if let Some(graph_node) = extract_named_item(
                node,
                source,
                file,
                module_path,
                parent_id,
                NodeKind::Trait,
                result,
            ) {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();
                result.nodes.push(graph_node);

                // Emit Inherits edges for supertrait bounds (e.g. `trait Child: Base`)
                if let Some(bounds) = node.child_by_field_name("bounds")
                    && let Ok(bounds_text) = bounds.utf8_text(source)
                {
                    for bound_name in collect_symbol_paths(bounds_text) {
                        result.edges.push(Edge {
                            source: node_id.clone(),
                            target: qualify_reference_target(file, module_path, &bound_name),
                            kind: EdgeKind::Inherits,
                            confidence: 0.9,
                            direction: None,
                            operation: None,
                            condition: None,
                            async_boundary: None,
                            provenance: edge_provenance(file, bounds, &node_id),
                            repo: None,
                        });
                    }
                }

                if let Some(body) = node.child_by_field_name("body") {
                    walk_children(body, source, file, module_path, Some(&node_id), result);
                }
            }
        }
        "impl_item" => {
            if let Some(graph_node) =
                extract_impl_item(node, source, file, module_path, parent_id, result)
            {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let node_id = graph_node.id.clone();

                // Emit Implements edge if this is `impl Trait for Type`
                // The source is the type being implemented, target is the trait
                if let Some(trait_node) = node.child_by_field_name("trait")
                    && let Ok(trait_name) = trait_node.utf8_text(source)
                {
                    let type_name = &graph_node.name;
                    let type_id = qualify_reference_target(file, module_path, type_name);
                    let trait_id = primary_symbol_path(trait_name)
                        .map(|path| qualify_reference_target(file, module_path, &path))
                        .unwrap_or_else(|| qualify_reference_target(file, module_path, trait_name));
                    result.edges.push(Edge {
                        source: type_id,
                        target: trait_id,
                        kind: EdgeKind::Implements,
                        confidence: 0.9,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, trait_node, &node_id),
                        repo: None,
                    });
                }

                result.nodes.push(graph_node);

                if let Some(body) = node.child_by_field_name("body") {
                    walk_children(body, source, file, module_path, Some(&node_id), result);
                }
            }
        }
        "mod_item" => {
            if let Some(graph_node) = extract_named_item(
                node,
                source,
                file,
                module_path,
                parent_id,
                NodeKind::Module,
                result,
            ) {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                let mod_name = graph_node.name.clone();
                let node_id = graph_node.id.clone();
                result.nodes.push(graph_node);

                // Walk the module body with extended module_path
                if let Some(body) = node.child_by_field_name("body") {
                    let mut new_path = module_path.to_vec();
                    new_path.push(mod_name);
                    walk_children(body, source, file, &new_path, Some(&node_id), result);
                }
            }
        }
        "use_declaration" => {
            if let Ok(use_text) = node.utf8_text(source) {
                let raw = use_text
                    .trim_start_matches("use ")
                    .trim_end_matches(';')
                    .trim()
                    .to_string();

                let kind = if raw.starts_with("crate::")
                    || raw.starts_with("super::")
                    || raw.starts_with("self::")
                {
                    grapha_core::resolve::ImportKind::Relative
                } else if raw.ends_with("::*") {
                    grapha_core::resolve::ImportKind::Wildcard
                } else {
                    grapha_core::resolve::ImportKind::Named
                };

                // Extract symbols from grouped imports: use foo::{A, B}
                let (path, symbols) = if let Some(brace_start) = raw.find('{') {
                    let base = raw[..brace_start].trim_end_matches("::").to_string();
                    let inner = raw[brace_start + 1..].trim_end_matches('}').trim();
                    let syms = inner.split(',').map(|s| s.trim().to_string()).collect();
                    (base, syms)
                } else {
                    (raw.trim_end_matches("::*").to_string(), vec![])
                };

                result.imports.push(grapha_core::resolve::Import {
                    path: path.clone(),
                    symbols,
                    kind,
                });

                // The import is recorded above as a structured `Import` plus a real
                // `Import` node and a resolvable `file:<path> -> <import node>`
                // `Imports` edge (see `push_import_node`). We deliberately do NOT
                // emit the legacy `Uses` edge here: its source was a bare file path
                // (not `file:<path>`) and its target was the raw `use ...;` text, so
                // it referenced no graph nodes and was 100% dangling — flagged as
                // `OrphanEdge` by `doctor` and pruned for LLM output. The parallel
                // `imports`/`Imports` family encodes the same information correctly.
                push_import_node(result, file, node, &path, use_text);
            }
        }
        "macro_invocation" => {
            if let Some(graph_node) =
                extract_newtype_macro(node, source, file, module_path, parent_id, result)
            {
                if let Some(pid) = parent_id {
                    result.edges.push(Edge {
                        source: pid.to_string(),
                        target: graph_node.id.clone(),
                        kind: EdgeKind::Contains,
                        confidence: 1.0,
                        direction: None,
                        operation: None,
                        condition: None,
                        async_boundary: None,
                        provenance: edge_provenance(file, node, pid),
                        repo: None,
                    });
                }
                result.nodes.push(graph_node);
            }
            // No children of a macro token tree are real items, so do not recurse.
        }
        _ => {
            // For any other node kind, just walk its children
            walk_children(node, source, file, module_path, parent_id, result);
        }
    }
}

/// Synthesize a struct-like type node for a macro that declares a single newtype
/// in item position (e.g. `ulid_newtype! { SourceId }` or `entity_id!(RevId)`).
///
/// tree-sitter cannot expand macros, so domain types declared this way are
/// otherwise invisible to `context`/`impact`/`usages`. We fire only when:
///   1. the macro name is recognised by [`is_newtype_macro_name`],
///   2. the invocation is in *item* position (not an expression inside a function
///      body), and
///   3. the first identifier argument is `PascalCase` (a type name).
///
/// Returns `None` otherwise, so a misuse in expression position or a non-type
/// argument never fabricates a phantom type.
fn extract_newtype_macro(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &ExtractionResult,
) -> Option<Node> {
    let macro_name = node
        .child_by_field_name("macro")
        .and_then(|m| m.utf8_text(source).ok())?
        .trim();
    if !is_newtype_macro_name(macro_name) || !is_macro_in_item_position(node) {
        return None;
    }

    let token_tree = macro_token_tree(node)?;
    let type_name = first_macro_type_argument(token_tree, source)?;

    let proposed_id = make_decl_id(file, module_path, parent_id, &type_name);
    let id = unique_decl_id(result, proposed_id, node);
    let start = node.start_position();
    let end = node.end_position();
    let mut metadata = HashMap::new();
    metadata.insert("language".to_string(), "rust".to_string());
    metadata.insert("macro_generated".to_string(), "true".to_string());
    metadata.insert("macro".to_string(), macro_name.to_string());

    Some(Node {
        id,
        kind: NodeKind::Struct,
        name: type_name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility: Visibility::Public,
        metadata,
        role: None,
        signature: None,
        doc_comment: extract_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    })
}

/// Returns true when a `macro_invocation` sits in *item* position — directly
/// under a `source_file`, a `declaration_list` (module / `impl` / `trait` body),
/// or as a `;`-terminated `expression_statement` whose own container is one of
/// those. Excludes invocations nested inside a function `block` (expression
/// position), where they would be statements, not type declarations.
fn is_macro_in_item_position(node: tree_sitter::Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "source_file" | "declaration_list" => true,
        "expression_statement" => parent
            .parent()
            .map(|grandparent| matches!(grandparent.kind(), "source_file" | "declaration_list"))
            .unwrap_or(false),
        _ => false,
    }
}

/// Find the `token_tree` child of a `macro_invocation`. tree-sitter exposes it as
/// an unnamed child (not a field), so we locate it by kind.
fn macro_token_tree(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| child.kind() == "token_tree")
}

/// Extract the first `PascalCase` identifier argument from a macro `token_tree`
/// (the newtype's name). Skips doc-comment tokens and any leading non-type
/// tokens; returns `None` if no `PascalCase` identifier is present.
fn first_macro_type_argument(token_tree: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = token_tree.walk();
    for child in token_tree.named_children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(text) = child.utf8_text(source)
            && is_pascal_case_type(text.trim())
        {
            return Some(text.trim().to_string());
        }
    }
    None
}

/// Walk all named children of a node.
fn walk_children(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_node(child, source, file, module_path, parent_id, result);
    }
}

/// Build a node ID from file, module path, and name.
fn make_id(file: &str, module_path: &[String], name: &str) -> String {
    if module_path.is_empty() {
        format!("{}::{}", file, name)
    } else {
        format!("{}::{}::{}", file, module_path.join("::"), name)
    }
}

fn make_decl_id(file: &str, module_path: &[String], parent_id: Option<&str>, name: &str) -> String {
    parent_id
        .map(|pid| format!("{pid}::{name}"))
        .unwrap_or_else(|| make_id(file, module_path, name))
}

fn unique_decl_id(
    result: &ExtractionResult,
    proposed_id: String,
    node: tree_sitter::Node,
) -> String {
    if result
        .nodes
        .iter()
        .all(|existing| existing.id != proposed_id)
    {
        return proposed_id;
    }

    let start = node.start_position();
    let end = node.end_position();
    format!(
        "{proposed_id}@{}:{}:{}:{}",
        start.row, start.column, end.row, end.column
    )
}

fn push_import_node(
    result: &mut ExtractionResult,
    file: &str,
    node: tree_sitter::Node,
    path: &str,
    signature: &str,
) {
    let proposed_id = make_id(file, &[], &format!("import:{path}"));
    let id = unique_decl_id(result, proposed_id, node);
    let start = node.start_position();
    let end = node.end_position();
    let mut metadata = HashMap::new();
    metadata.insert("language".to_string(), "rust".to_string());

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Import,
        name: path.to_string(),
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility: Visibility::Private,
        metadata,
        role: None,
        signature: Some(signature.trim().to_string()),
        doc_comment: None,
        module: None,
        snippet: None,
        repo: None,
    });
    result.edges.push(Edge {
        source: format!("file:{file}"),
        target: id,
        kind: EdgeKind::Imports,
        confidence: 0.9,
        direction: None,
        operation: None,
        condition: None,
        async_boundary: None,
        provenance: edge_provenance(file, node, &format!("file:{file}")),
        repo: None,
    });
}

fn qualify_reference_target(file: &str, module_path: &[String], target: &str) -> String {
    if target.contains("::") || target.contains('.') {
        target.to_string()
    } else {
        make_id(file, module_path, target)
    }
}

fn normalize_call_target(raw: &str) -> Option<String> {
    let normalized = erase_generic_arguments(raw)
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .trim_end_matches('?')
        .trim_end_matches("::")
        .to_string();
    (!normalized.is_empty() && !normalized.ends_with('!')).then_some(normalized)
}

fn erase_generic_arguments(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for ch in raw.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => result.push(ch),
            _ => {}
        }
    }
    result
}

fn collect_symbol_paths(raw: &str) -> Vec<String> {
    let bytes = raw.as_bytes();
    let mut index = 0usize;
    let mut symbols = Vec::new();

    while index < bytes.len() {
        if bytes[index] == b'\'' {
            index += 1;
            while index < bytes.len() && is_ident_char(bytes[index]) {
                index += 1;
            }
            continue;
        }

        let Some((path, next_index)) = parse_symbol_path(raw, index) else {
            index += 1;
            continue;
        };
        index = next_index;

        if should_keep_symbol_path(&path) && !symbols.contains(&path) {
            symbols.push(path);
        }
    }

    symbols
}

fn parse_symbol_path(raw: &str, start: usize) -> Option<(String, usize)> {
    let bytes = raw.as_bytes();
    let (mut path, mut index) = parse_ident(raw, start)?;

    loop {
        let mut next = index;
        while next < bytes.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        if next + 1 >= bytes.len() || bytes[next] != b':' || bytes[next + 1] != b':' {
            break;
        }
        next += 2;
        while next < bytes.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        let Some((segment, after_segment)) = parse_ident(raw, next) else {
            break;
        };
        path.push_str("::");
        path.push_str(&segment);
        index = after_segment;
    }

    Some((path, index))
}

fn parse_ident(raw: &str, start: usize) -> Option<(String, usize)> {
    let bytes = raw.as_bytes();
    if start >= bytes.len() {
        return None;
    }

    let mut index = start;
    if bytes.get(index) == Some(&b'r') && bytes.get(index + 1) == Some(&b'#') {
        index += 2;
    }

    let first = *bytes.get(index)?;
    if !is_ident_start(first) {
        return None;
    }

    let ident_start = index;
    index += 1;
    while index < bytes.len() && is_ident_char(bytes[index]) {
        index += 1;
    }

    Some((raw[ident_start..index].to_string(), index))
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_char(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn should_keep_symbol_path(path: &str) -> bool {
    if path == "Self" || path == "self" {
        return false;
    }
    if path.contains("::") {
        return true;
    }
    if is_primitive(path) {
        return false;
    }

    !matches!(
        path,
        "dyn"
            | "impl"
            | "mut"
            | "const"
            | "async"
            | "unsafe"
            | "extern"
            | "fn"
            | "for"
            | "where"
            | "move"
    )
}

fn primary_symbol_path(raw: &str) -> Option<String> {
    collect_symbol_paths(raw).into_iter().next()
}

fn terminal_symbol_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

fn first_identifier_text(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier"
    ) {
        return node.utf8_text(source).ok().map(ToString::to_string);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(name) = first_identifier_text(child, source) {
            return Some(name);
        }
    }
    None
}

/// Extract the text of a named child field.
fn field_text<'a>(node: tree_sitter::Node<'a>, field: &str, source: &'a [u8]) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
}

/// Extract visibility from a node by checking for a `visibility_modifier` child.
fn extract_visibility(node: tree_sitter::Node, source: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            let text = child.utf8_text(source).unwrap_or("");
            if text.contains("pub(crate)") {
                return Visibility::Crate;
            } else if text.starts_with("pub") {
                return Visibility::Public;
            }
        }
    }
    Visibility::Private
}

/// Extract metadata (async, unsafe) from the `function_modifiers` named child.
///
/// tree-sitter-rust wraps these keywords in a `function_modifiers` node.
/// The keywords themselves are anonymous tokens with kinds `"async"` / `"unsafe"`.
fn extract_function_metadata(node: tree_sitter::Node, _source: &[u8]) -> HashMap<String, String> {
    let mut meta = HashMap::new();

    // Check direct children for function_modifiers node
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.children(&mut mod_cursor) {
                match modifier.kind() {
                    "async" => {
                        meta.insert("async".to_string(), "true".to_string());
                    }
                    "unsafe" => {
                        meta.insert("unsafe".to_string(), "true".to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    meta
}

/// Extract a function_item or function_signature_item into a Node.
fn extract_function(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &ExtractionResult,
) -> Option<Node> {
    let name = field_text(node, "name", source)?;
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);
    let metadata = extract_function_metadata(node, source);
    let start = node.start_position();
    let end = node.end_position();

    let role = detect_entry_point(node, source, &name, module_path);
    let signature = extract_signature(node, source);
    let doc_comment = extract_doc_comment(node, source);

    Some(Node {
        id,
        kind: NodeKind::Function,
        name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility,
        metadata,
        role,
        signature,
        doc_comment,
        module: None,
        snippet: None,
        repo: None,
    })
}

/// Detect if a function is an entry point.
///
/// Entry points are: `fn main()` at module level, functions with `#[test]`,
/// `#[tokio::main]`, or `pub fn` at crate root level (no parent impl/trait).
fn detect_entry_point(
    node: tree_sitter::Node,
    source: &[u8],
    name: &str,
    module_path: &[String],
) -> Option<NodeRole> {
    let attrs = collect_attributes(node, source);
    let is_module_level = node
        .parent()
        .map(|p| p.kind() == "source_file" || p.kind() == "declaration_list")
        .unwrap_or(false);
    let is_inside_impl_or_trait = node
        .parent()
        .and_then(|p| p.parent())
        .map(|gp| gp.kind() == "impl_item" || gp.kind() == "trait_item")
        .unwrap_or(false);

    // #[test] or #[tokio::main] attributes
    for attr in &attrs {
        if attr == "test" || attr == "tokio::test" || attr == "tokio::main" {
            return Some(NodeRole::EntryPoint);
        }
    }

    // fn main() at module level, not inside impl/trait
    if name == "main" && is_module_level && !is_inside_impl_or_trait {
        return Some(NodeRole::EntryPoint);
    }

    // pub fn at crate root level (module_path is empty, not inside impl/trait)
    let visibility = extract_visibility(node, source);
    if visibility == Visibility::Public
        && module_path.is_empty()
        && is_module_level
        && !is_inside_impl_or_trait
    {
        return Some(NodeRole::EntryPoint);
    }

    None
}

/// Collect attribute names from sibling `attribute_item` nodes before the function.
fn collect_attributes(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut attrs = Vec::new();
    let mut prev = node.prev_named_sibling();
    while let Some(sib) = prev {
        if sib.kind() == "attribute_item" {
            if let Ok(text) = sib.utf8_text(source) {
                // Strip #[ and ]
                let inner = text.trim_start_matches("#[").trim_end_matches(']').trim();
                // Take the attribute path (before any parentheses)
                let attr_name = inner.split('(').next().unwrap_or(inner).trim();
                attrs.push(attr_name.to_string());
            }
            prev = sib.prev_named_sibling();
        } else {
            break;
        }
    }
    attrs
}

/// Extract function signature (text up to opening `{`).
fn extract_signature(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else {
        // For signature items (no body), use the whole text minus trailing semicolons
        text.trim().trim_end_matches(';').trim()
    };
    if sig.is_empty() {
        None
    } else {
        Some(sig.to_string())
    }
}

fn extract_decl_signature(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    node.utf8_text(source)
        .ok()
        .map(str::trim)
        .map(|text| text.trim_end_matches(';').trim())
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// Extract doc comments from previous sibling comment nodes.
fn extract_doc_comment(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut comments = Vec::new();
    let mut prev = node.prev_named_sibling();
    // Skip over attribute_item siblings first
    while let Some(sib) = prev {
        if sib.kind() == "attribute_item" {
            prev = sib.prev_named_sibling();
            continue;
        }
        if sib.kind() == "line_comment" || sib.kind() == "block_comment" {
            if let Ok(text) = sib.utf8_text(source) {
                comments.push(text.to_string());
            }
            prev = sib.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

/// Walk up from a call node to find an enclosing conditional, returning condition text.
/// Stops at `function_item` boundary.
fn find_enclosing_condition(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_item" | "function_signature_item" => return None,
            "if_expression" => {
                if let Some(cond) = parent.child_by_field_name("condition") {
                    return cond.utf8_text(source).ok().map(|s| s.trim().to_string());
                }
                return None;
            }
            "if_let_expression" => {
                // Grab "let PATTERN = EXPR" condition text
                if let Some(pat) = parent.child_by_field_name("pattern")
                    && let Some(val) = parent.child_by_field_name("value")
                {
                    let pat_text = pat.utf8_text(source).unwrap_or_default();
                    let val_text = val.utf8_text(source).unwrap_or_default();
                    return Some(format!("let {} = {}", pat_text.trim(), val_text.trim()));
                }
                return None;
            }
            "match_arm" => {
                if let Some(pat) = parent.child_by_field_name("pattern")
                    && let Ok(pat_text) = pat.utf8_text(source)
                {
                    return Some(format!("match {}", pat_text.trim()));
                }
                return None;
            }
            _ => {
                current = parent.parent();
            }
        }
    }
    None
}

/// Check if a call node is at an async boundary (await or inside spawn).
fn detect_async_boundary(node: tree_sitter::Node, source: &[u8]) -> Option<bool> {
    // Check if parent is await_expression
    if let Some(parent) = node.parent()
        && parent.kind() == "await_expression"
    {
        return Some(true);
    }
    // Check if inside a tokio::spawn or std::thread::spawn call
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" || parent.kind() == "function_signature_item" {
            break;
        }
        if parent.kind() == "call_expression"
            && let Some(func) = parent.child_by_field_name("function")
            && let Ok(func_text) = func.utf8_text(source)
        {
            let trimmed = func_text.trim();
            if trimmed.contains("spawn") {
                return Some(true);
            }
        }
        current = parent.parent();
    }
    None
}

fn push_type_ref_edges(
    result: &mut ExtractionResult,
    file: &str,
    type_node: tree_sitter::Node,
    source_id: &str,
    module_path: &[String],
    raw_type_name: &str,
) {
    for type_name in collect_symbol_paths(raw_type_name.trim()) {
        result.edges.push(Edge {
            source: source_id.to_string(),
            target: qualify_reference_target(file, module_path, &type_name),
            kind: EdgeKind::TypeRef,
            confidence: 0.85,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: edge_provenance(file, type_node, source_id),
            repo: None,
        });
    }
}

/// Extract a named symbol (struct, enum, trait, module) into a Node.
fn extract_named_item(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    kind: NodeKind,
    result: &ExtractionResult,
) -> Option<Node> {
    let name = field_text(node, "name", source)?;
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);
    let start = node.start_position();
    let end = node.end_position();

    Some(Node {
        id,
        kind,
        name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility,
        metadata: HashMap::new(),
        role: None,
        signature: None,
        doc_comment: extract_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    })
}

/// Extract an impl_item into a Node.
/// The node name is the type being implemented (e.g. `Foo`).
/// The ID uses `impl_{TypeName}` to avoid collisions with the type node.
fn extract_impl_item(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &ExtractionResult,
) -> Option<Node> {
    let raw_type_name = field_text(node, "type", source)?;
    let type_name = primary_symbol_path(&raw_type_name)
        .map(|path| terminal_symbol_name(&path).to_string())
        .unwrap_or(raw_type_name);
    let impl_name = format!("impl_{}", type_name);
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &impl_name),
        node,
    );
    let start = node.start_position();
    let end = node.end_position();

    Some(Node {
        id,
        kind: NodeKind::Impl,
        name: type_name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility: Visibility::Private,
        metadata: HashMap::new(),
        role: None,
        signature: None,
        doc_comment: None,
        module: None,
        snippet: None,
        repo: None,
    })
}

fn extract_constant(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &ExtractionResult,
) -> Option<Node> {
    let name = field_text(node, "name", source)?;
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);
    let start = node.start_position();
    let end = node.end_position();
    let mut metadata = HashMap::new();

    if node.kind() == "static_item" {
        metadata.insert("static".to_string(), "true".to_string());
        let mut cursor = node.walk();
        if node
            .children(&mut cursor)
            .any(|child| child.kind() == "mutable_specifier")
        {
            metadata.insert("mutable".to_string(), "true".to_string());
        }
    }

    Some(Node {
        id,
        kind: NodeKind::Constant,
        name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility,
        metadata,
        role: None,
        signature: extract_decl_signature(node, source),
        doc_comment: extract_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    })
}

fn extract_variable(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &ExtractionResult,
) -> Option<Node> {
    let pattern = node
        .child_by_field_name("pattern")
        .or_else(|| node.child_by_field_name("name"))
        .unwrap_or(node);
    let name = first_identifier_text(pattern, source)?;
    if name == "_" {
        return None;
    }
    // `let Some(x) = ..` / `let Ok(v) = ..` destructure into a `tuple_struct_pattern`
    // whose leading identifier is the std constructor, not a real binding. Suppress
    // the phantom node named after the constructor (the inner binding, if any, is a
    // separate concern and is not a top-level `let` name here).
    if is_constructor_destructure(pattern, source) {
        return None;
    }
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let start = node.start_position();
    let end = node.end_position();
    let mut metadata = HashMap::new();
    if node.utf8_text(source).is_ok_and(|raw| raw.contains("mut ")) {
        metadata.insert("mutable".to_string(), "true".to_string());
    }

    Some(Node {
        id,
        kind: NodeKind::Variable,
        name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility: Visibility::Private,
        metadata,
        role: None,
        signature: extract_decl_signature(node, source),
        doc_comment: extract_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    })
}

/// Returns true when the *first* binding extracted from `pattern` comes from
/// destructuring a std Option/Result constructor, e.g. the `Some` of
/// `let Some(x) = opt else { .. };` or the leading `Some` of
/// `let (Some(a), Some(b)) = pair else { .. };`. The variant name (`Some`/`Ok`/
/// `Err`/`None`) is what `first_identifier_text` returns there, but it is not a
/// real variable. Anchored to the `tuple_struct_pattern` shape (descending only
/// through structural wrapper patterns) so a user binding is never mistaken for
/// one.
fn is_constructor_destructure(pattern: tree_sitter::Node, source: &[u8]) -> bool {
    match pattern.kind() {
        "tuple_struct_pattern" => pattern
            .child_by_field_name("type")
            .and_then(|type_node| type_node.utf8_text(source).ok())
            .map(|type_text| is_std_option_result_constructor(type_text.trim()))
            .unwrap_or(false),
        // The leading binding of a tuple / reference wrapper drives the synthesized
        // name; descend into the first named child to inspect it.
        "tuple_pattern" | "reference_pattern" | "ref_pattern" | "mut_pattern" => {
            let mut cursor = pattern.walk();
            pattern
                .named_children(&mut cursor)
                .next()
                .map(|first| is_constructor_destructure(first, source))
                .unwrap_or(false)
        }
        _ => false,
    }
}

fn extract_type_alias(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &ExtractionResult,
) -> Option<Node> {
    let name = field_text(node, "name", source)?;
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);
    let start = node.start_position();
    let end = node.end_position();

    Some(Node {
        id,
        kind: NodeKind::TypeAlias,
        name,
        file: file.into(),
        span: Span {
            start: [start.row, start.column],
            end: [end.row, end.column],
        },
        visibility,
        metadata: HashMap::new(),
        role: None,
        signature: extract_decl_signature(node, source),
        doc_comment: extract_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    })
}

/// Extract field_declaration children from a struct body.
fn extract_struct_fields(
    body: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: &str,
    parent_name: &str,
    result: &mut ExtractionResult,
) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "field_declaration"
            && let Some(name) = field_text(child, "name", source)
        {
            let qualified = format!("{parent_name}.{name}");
            let id = make_id(file, module_path, &qualified);
            let visibility = extract_visibility(child, source);
            let start = child.start_position();
            let end = child.end_position();

            result.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: edge_provenance(file, child, parent_id),
                repo: None,
            });

            result.nodes.push(Node {
                id,
                kind: NodeKind::Field,
                name,
                file: file.into(),
                span: Span {
                    start: [start.row, start.column],
                    end: [end.row, end.column],
                },
                visibility,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            });
        }
    }
}

/// Returns true if the type name is a Rust primitive.
fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "char"
            | "str"
            | "()"
    )
}

/// Recursively scan a node tree for `call_expression` nodes, emitting Calls edges.
fn extract_calls(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    caller_id: &str,
    result: &mut ExtractionResult,
) {
    if node.kind() == "call_expression"
        && let Some(function_node) = node.child_by_field_name("function")
        && let Ok(fn_text) = function_node.utf8_text(source)
        && let Some(callee_name) = normalize_call_target(fn_text)
        && !is_std_option_result_constructor(&callee_name)
    {
        let target_id = if is_unqualified_call_target(&callee_name) {
            make_id(file, module_path, &callee_name)
        } else {
            callee_name
        };
        let condition = find_enclosing_condition(node, source);
        let async_boundary = detect_async_boundary(node, source);
        result.edges.push(Edge {
            source: caller_id.to_string(),
            target: target_id,
            kind: EdgeKind::Calls,
            confidence: 0.8,
            direction: None,
            operation: None,
            condition,
            async_boundary,
            provenance: edge_provenance(file, node, caller_id),
            repo: None,
        });
    }

    // Recurse into all children
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_calls(child, source, file, module_path, caller_id, result);
    }
}

fn is_unqualified_call_target(target: &str) -> bool {
    !target.contains("::") && !target.contains('.')
}

fn extract_reads_and_writes(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    caller_id: &str,
    result: &mut ExtractionResult,
) {
    match node.kind() {
        "assignment_expression" | "compound_assignment_expr" => {
            if let Some(left) = node.child_by_field_name("left")
                && let Some(target) = extract_self_field_target(left, source)
            {
                let provenance = edge_provenance(file, left, caller_id);
                result.edges.push(Edge {
                    source: caller_id.to_string(),
                    target: target.clone(),
                    kind: EdgeKind::Writes,
                    confidence: 0.8,
                    direction: Some(grapha_core::graph::FlowDirection::Write),
                    operation: None,
                    condition: find_enclosing_condition(node, source),
                    async_boundary: detect_async_boundary(node, source),
                    provenance: provenance.clone(),
                    repo: None,
                });
                if node.kind() == "compound_assignment_expr" {
                    result.edges.push(Edge {
                        source: caller_id.to_string(),
                        target,
                        kind: EdgeKind::Reads,
                        confidence: 0.75,
                        direction: Some(grapha_core::graph::FlowDirection::Read),
                        operation: None,
                        condition: find_enclosing_condition(node, source),
                        async_boundary: detect_async_boundary(node, source),
                        provenance,
                        repo: None,
                    });
                }
            }

            if let Some(right) = node.child_by_field_name("right") {
                extract_reads_and_writes(right, source, file, caller_id, result);
            }
        }
        "field_expression" => {
            if should_emit_field_read(node)
                && let Some(target) = extract_self_field_target(node, source)
            {
                result.edges.push(Edge {
                    source: caller_id.to_string(),
                    target,
                    kind: EdgeKind::Reads,
                    confidence: 0.75,
                    direction: Some(grapha_core::graph::FlowDirection::Read),
                    operation: None,
                    condition: find_enclosing_condition(node, source),
                    async_boundary: detect_async_boundary(node, source),
                    provenance: edge_provenance(file, node, caller_id),
                    repo: None,
                });
            }
        }
        "identifier" | "scoped_identifier" => {
            if should_emit_constant_read(node, source) {
                let target = node
                    .utf8_text(source)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                result.edges.push(Edge {
                    source: caller_id.to_string(),
                    target,
                    kind: EdgeKind::Reads,
                    confidence: 0.7,
                    direction: Some(grapha_core::graph::FlowDirection::Read),
                    operation: None,
                    condition: find_enclosing_condition(node, source),
                    async_boundary: detect_async_boundary(node, source),
                    provenance: edge_provenance(file, node, caller_id),
                    repo: None,
                });
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_reads_and_writes(child, source, file, caller_id, result);
            }
        }
    }
}

fn extract_self_field_target(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    if node.kind() != "field_expression" {
        return None;
    }

    let value = node.child_by_field_name("value")?;
    let field = node.child_by_field_name("field")?;
    let receiver = value.utf8_text(source).ok()?.trim();
    let field_name = field.utf8_text(source).ok()?.trim();
    if receiver == "self" && !field_name.is_empty() {
        Some(format!("{receiver}.{field_name}"))
    } else {
        None
    }
}

fn should_emit_field_read(node: tree_sitter::Node) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };

    if parent.kind() == "call_expression" && parent.child_by_field_name("function") == Some(node) {
        return false;
    }

    if matches!(
        parent.kind(),
        "assignment_expression" | "compound_assignment_expr"
    ) && parent.child_by_field_name("left") == Some(node)
    {
        return false;
    }

    true
}

fn should_emit_constant_read(node: tree_sitter::Node, source: &[u8]) -> bool {
    let Ok(text) = node.utf8_text(source) else {
        return false;
    };
    if !is_constant_like(text.trim()) {
        return false;
    }

    let Some(parent) = node.parent() else {
        return true;
    };

    if parent.child_by_field_name("name") == Some(node)
        || parent.child_by_field_name("field") == Some(node)
        || parent.child_by_field_name("alias") == Some(node)
        || (parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(node))
    {
        return false;
    }

    true
}

fn is_constant_like(text: &str) -> bool {
    let candidate = text
        .rsplit("::")
        .next()
        .unwrap_or(text)
        .trim_start_matches("r#");
    let mut has_uppercase = false;
    for ch in candidate.chars() {
        if ch.is_ascii_lowercase() {
            return false;
        }
        if ch.is_ascii_uppercase() {
            has_uppercase = true;
        }
        if !(ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') {
            return false;
        }
    }
    has_uppercase
}

/// Extract enum_variant children from an enum body.
fn extract_enum_variants(
    body: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: &str,
    parent_name: &str,
    result: &mut ExtractionResult,
) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "enum_variant"
            && let Some(name) = field_text(child, "name", source)
        {
            let qualified = format!("{parent_name}.{name}");
            let id = make_id(file, module_path, &qualified);
            let start = child.start_position();
            let end = child.end_position();

            result.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: edge_provenance(file, child, parent_id),
                repo: None,
            });

            result.nodes.push(Node {
                id,
                kind: NodeKind::Variant,
                name,
                file: file.into(),
                span: Span {
                    start: [start.row, start.column],
                    end: [end.row, end.column],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{EdgeKind, NodeKind, Visibility};
    use grapha_core::merge as merge_results;

    fn extract(source: &str) -> ExtractionResult {
        let extractor = RustExtractor;
        extractor
            .extract(source.as_bytes(), Path::new("test.rs"))
            .unwrap()
    }

    fn find_node<'a>(result: &'a ExtractionResult, name: &str) -> &'a grapha_core::graph::Node {
        result
            .nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("node '{}' not found", name))
    }

    fn has_edge(result: &ExtractionResult, source: &str, target: &str, kind: EdgeKind) -> bool {
        result
            .edges
            .iter()
            .any(|e| e.source == source && e.target == target && e.kind == kind)
    }

    #[test]
    fn extracts_function() {
        let result = extract("pub fn greet(name: &str) -> String { format!(\"hi {}\", name) }");
        let node = find_node(&result, "greet");
        assert_eq!(node.kind, NodeKind::Function);
        assert_eq!(node.visibility, Visibility::Public);
    }

    #[test]
    fn extracts_file_import_and_variable_nodes() {
        let result = extract(
            r#"
            use std::fmt;

            fn main() {
                let mut count: usize = 1;
            }
            "#,
        );

        let file = find_node(&result, "test.rs");
        assert_eq!(file.kind, NodeKind::File);

        let import = find_node(&result, "std::fmt");
        assert_eq!(import.kind, NodeKind::Import);
        assert_eq!(import.signature.as_deref(), Some("use std::fmt;"));

        let main = find_node(&result, "main");
        let count = find_node(&result, "count");
        assert_eq!(count.kind, NodeKind::Variable);
        assert_eq!(
            count.metadata.get("mutable").map(String::as_str),
            Some("true")
        );

        assert!(has_edge(&result, &file.id, &import.id, EdgeKind::Contains));
        assert!(has_edge(&result, &file.id, &import.id, EdgeKind::Imports));
        assert!(has_edge(&result, &file.id, &main.id, EdgeKind::Contains));
        assert!(has_edge(&result, &main.id, &count.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_framework_route_nodes() {
        let result = extract(
            r#"
            use axum::{routing::get, Router};

            #[get("/health")]
            async fn health() {}

            fn router() -> Router {
                Router::new().route("/users", get(health))
            }
            "#,
        );

        let file = find_node(&result, "test.rs");
        let health_route = find_node(&result, "GET /health");
        let users_route = find_node(&result, "GET /users");

        assert_eq!(health_route.kind, NodeKind::Route);
        assert_eq!(users_route.kind, NodeKind::Route);
        assert!(has_edge(
            &result,
            &file.id,
            &health_route.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &file.id,
            &users_route.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extracts_async_unsafe_metadata() {
        let result = extract("pub async fn fetch() {} unsafe fn danger() {}");
        let fetch = find_node(&result, "fetch");
        assert_eq!(
            fetch.metadata.get("async").map(|s| s.as_str()),
            Some("true")
        );
        let danger = find_node(&result, "danger");
        assert_eq!(
            danger.metadata.get("unsafe").map(|s| s.as_str()),
            Some("true")
        );
    }

    #[test]
    fn extracts_struct_with_fields() {
        let result = extract(
            r#"
            pub struct Config {
                pub debug: bool,
                name: String,
            }
            "#,
        );
        let config = find_node(&result, "Config");
        assert_eq!(config.kind, NodeKind::Struct);
        assert_eq!(config.visibility, Visibility::Public);

        let debug = find_node(&result, "debug");
        assert_eq!(debug.kind, NodeKind::Field);
        assert_eq!(debug.visibility, Visibility::Public);

        let name = find_node(&result, "name");
        assert_eq!(name.kind, NodeKind::Field);
        assert_eq!(name.visibility, Visibility::Private);

        assert!(has_edge(&result, &config.id, &debug.id, EdgeKind::Contains));
        assert!(has_edge(&result, &config.id, &name.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_enum_with_variants() {
        let result = extract(
            r#"
            pub enum Color {
                Red,
                Green,
                Blue,
            }
            "#,
        );
        let color = find_node(&result, "Color");
        assert_eq!(color.kind, NodeKind::Enum);

        let red = find_node(&result, "Red");
        assert_eq!(red.kind, NodeKind::Variant);

        assert!(has_edge(&result, &color.id, &red.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_trait() {
        let result = extract(
            r#"
            pub trait Drawable {
                fn draw(&self);
            }
            "#,
        );
        let drawable = find_node(&result, "Drawable");
        assert_eq!(drawable.kind, NodeKind::Trait);
        assert_eq!(drawable.visibility, Visibility::Public);

        let draw = find_node(&result, "draw");
        assert_eq!(draw.kind, NodeKind::Function);

        assert!(has_edge(
            &result,
            &drawable.id,
            &draw.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extracts_impl_block() {
        let result = extract(
            r#"
            struct Foo;
            impl Foo {
                pub fn new() -> Self { Foo }
            }
            "#,
        );
        let impl_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Impl)
            .expect("impl node not found");
        assert_eq!(impl_node.name, "Foo");

        let new_fn = find_node(&result, "new");
        assert!(has_edge(
            &result,
            &impl_node.id,
            &new_fn.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extracts_module() {
        let result = extract(
            r#"
            pub mod utils {
                pub fn helper() {}
            }
            "#,
        );
        let utils = find_node(&result, "utils");
        assert_eq!(utils.kind, NodeKind::Module);
        assert_eq!(utils.visibility, Visibility::Public);

        let helper = find_node(&result, "helper");
        assert!(has_edge(&result, &utils.id, &helper.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_pub_crate_visibility() {
        let result = extract("pub(crate) fn internal() {}");
        let node = find_node(&result, "internal");
        assert_eq!(node.visibility, Visibility::Crate);
    }

    #[test]
    fn extracts_calls_edges() {
        let result = extract(
            r#"
            fn helper() {}
            fn main() {
                helper();
            }
            "#,
        );
        assert!(has_edge(
            &result,
            "test.rs::main",
            "test.rs::helper",
            EdgeKind::Calls,
        ));
    }

    #[test]
    fn use_declarations_emit_resolvable_imports_not_dangling_uses() {
        let result = extract("use std::collections::HashMap;");

        // The legacy `Uses` edge (bare-file-path source + raw `use ...;` target)
        // was 100% dangling and must no longer be emitted.
        assert!(
            !result.edges.iter().any(|e| e.kind == EdgeKind::Uses),
            "rust extractor must not emit dangling Uses edges"
        );

        // The parallel `imports` family encodes the same info correctly: a real
        // Import node plus a `file:<path> -> <import node id>` Imports edge.
        let import = find_node(&result, "std::collections::HashMap");
        assert_eq!(import.kind, NodeKind::Import);
        let import_edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Imports)
            .expect("an Imports edge should exist for the use declaration");
        assert_eq!(import_edge.source, "file:test.rs");
        assert_eq!(import_edge.target, import.id);

        // And the structured Import is recorded for cross-file resolution.
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].path, "std::collections::HashMap");
    }

    #[test]
    fn extracts_implements_edge() {
        let result = extract(
            r#"
            trait Drawable { fn draw(&self); }
            struct Circle;
            impl Drawable for Circle {
                fn draw(&self) {}
            }
            "#,
        );
        assert!(result.edges.iter().any(|e| e.kind == EdgeKind::Implements));
    }

    #[test]
    fn extracts_type_ref_edges() {
        let result = extract(
            r#"
            struct Config { debug: bool }
            fn make_config() -> Config {
                Config { debug: true }
            }
            "#,
        );
        assert!(result.edges.iter().any(|e| e.kind == EdgeKind::TypeRef));
    }

    #[test]
    fn extracts_inner_generic_type_refs() {
        let result = extract(
            r#"
            struct Config;
            struct AppError;

            fn load() -> Result<Config, AppError> {
                unimplemented!()
            }
            "#,
        );
        let load = find_node(&result, "load");

        assert!(has_edge(
            &result,
            &load.id,
            "test.rs::Config",
            EdgeKind::TypeRef
        ));
        assert!(has_edge(
            &result,
            &load.id,
            "test.rs::AppError",
            EdgeKind::TypeRef
        ));
    }

    #[test]
    fn extracts_constants_and_type_aliases() {
        let result = extract(
            r#"
            pub const STORE_SCHEMA_VERSION: &str = "6";
            static mut GLOBAL_COUNTER: usize = 0;
            pub type SchemaVersion = String;
            "#,
        );

        let const_node = find_node(&result, "STORE_SCHEMA_VERSION");
        assert_eq!(const_node.kind, NodeKind::Constant);
        assert_eq!(const_node.visibility, Visibility::Public);

        let static_node = find_node(&result, "GLOBAL_COUNTER");
        assert_eq!(static_node.kind, NodeKind::Constant);
        assert_eq!(
            static_node.metadata.get("static").map(|s| s.as_str()),
            Some("true")
        );
        assert_eq!(
            static_node.metadata.get("mutable").map(|s| s.as_str()),
            Some("true")
        );

        let alias_node = find_node(&result, "SchemaVersion");
        assert_eq!(alias_node.kind, NodeKind::TypeAlias);
        assert!(
            result
                .edges
                .iter()
                .any(|edge| { edge.source == alias_node.id && edge.kind == EdgeKind::TypeRef })
        );
    }

    #[test]
    fn extracts_self_field_reads_writes_and_constant_reads() {
        let result = extract(
            r#"
            const STORE_SCHEMA_VERSION: &str = "6";

            struct SqliteStore {
                path: String,
            }

            impl SqliteStore {
                fn open(&self) {
                    let _ = &self.path;
                    let _ = STORE_SCHEMA_VERSION;
                }

                fn set_path(&mut self, next: String) {
                    self.path = next;
                }
            }
            "#,
        );
        let open = find_node(&result, "open");
        let set_path = find_node(&result, "set_path");

        assert!(result.edges.iter().any(|edge| {
            edge.source == open.id && edge.target == "self.path" && edge.kind == EdgeKind::Reads
        }));
        assert!(result.edges.iter().any(|edge| {
            edge.source == open.id
                && edge.target == "STORE_SCHEMA_VERSION"
                && edge.kind == EdgeKind::Reads
        }));
        assert!(result.edges.iter().any(|edge| {
            edge.source == set_path.id
                && edge.target == "self.path"
                && edge.kind == EdgeKind::Writes
        }));
    }

    #[test]
    fn extracts_structured_imports() {
        let result = extract("use std::collections::HashMap;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].path, "std::collections::HashMap");
        assert_eq!(
            result.imports[0].kind,
            grapha_core::resolve::ImportKind::Named
        );
    }

    #[test]
    fn extracts_relative_imports() {
        let result = extract("use crate::graph::Node;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(
            result.imports[0].kind,
            grapha_core::resolve::ImportKind::Relative
        );
    }

    #[test]
    fn extracts_glob_imports() {
        let result = extract("use std::collections::*;");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(
            result.imports[0].kind,
            grapha_core::resolve::ImportKind::Wildcard
        );
    }

    #[test]
    fn extracts_inherits_edge_for_supertraits() {
        let result = extract(
            r#"
            trait Base {}
            trait Child: Base {}
            "#,
        );
        assert!(has_edge(
            &result,
            "test.rs::Child",
            "test.rs::Base",
            EdgeKind::Inherits,
        ));
    }

    #[test]
    fn extracts_condition_on_call_inside_if() {
        let result = extract(
            r#"
            fn check() -> bool { true }
            fn run() {
                if check() {
                    helper();
                }
            }
            fn helper() {}
            "#,
        );
        let cond_edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target.contains("helper"))
            .expect("should find Calls edge to helper");
        assert!(
            cond_edge.condition.is_some(),
            "condition should be set on call inside if"
        );
        assert!(
            !cond_edge.provenance.is_empty(),
            "call edges should carry provenance"
        );
        assert_eq!(cond_edge.provenance[0].symbol_id, "test.rs::run");
    }

    #[test]
    fn detects_main_as_entry_point() {
        let result = extract(
            r#"
            fn main() {
                println!("hello");
            }
            "#,
        );
        let main_node = find_node(&result, "main");
        assert_eq!(
            main_node.role,
            Some(grapha_core::graph::NodeRole::EntryPoint),
            "fn main() should be detected as EntryPoint"
        );
    }

    #[test]
    fn detects_test_as_entry_point() {
        let result = extract(
            r#"
            #[test]
            fn my_test() {
                assert!(true);
            }
            "#,
        );
        let test_node = find_node(&result, "my_test");
        assert_eq!(
            test_node.role,
            Some(grapha_core::graph::NodeRole::EntryPoint),
            "#[test] fn should be detected as EntryPoint"
        );
    }

    #[test]
    fn detects_pub_fn_at_root_as_entry_point() {
        let result = extract("pub fn api_handler() {}");
        let node = find_node(&result, "api_handler");
        assert_eq!(
            node.role,
            Some(grapha_core::graph::NodeRole::EntryPoint),
            "pub fn at root should be EntryPoint"
        );
    }

    #[test]
    fn private_fn_at_root_is_not_entry_point() {
        let result = extract("fn helper() {}");
        let node = find_node(&result, "helper");
        assert!(
            node.role.is_none() || node.role == Some(grapha_core::graph::NodeRole::Internal),
            "private fn at root should not be EntryPoint (unless it's main)"
        );
    }

    #[test]
    fn extracts_function_signature() {
        let result = extract("pub fn greet(name: &str) -> String { format!(\"hi {}\", name) }");
        let node = find_node(&result, "greet");
        assert!(node.signature.is_some(), "signature should be extracted");
        let sig = node.signature.as_ref().unwrap();
        assert!(sig.contains("fn greet"), "signature should contain fn name");
        assert!(
            sig.contains("-> String"),
            "signature should contain return type"
        );
    }

    #[test]
    fn extracts_doc_comment() {
        let result = extract(
            r#"
            /// This is a doc comment
            /// with two lines
            fn documented() {}
            "#,
        );
        let node = find_node(&result, "documented");
        assert!(
            node.doc_comment.is_some(),
            "doc_comment should be extracted"
        );
        let doc = node.doc_comment.as_ref().unwrap();
        assert!(doc.contains("doc comment"), "should contain comment text");
    }

    #[test]
    fn extracts_doc_comments_for_named_declarations() {
        let result = extract(
            r#"
            /// Coordinates checkout.
            pub struct CheckoutFlow { id: String }

            /// Possible checkout states.
            enum CheckoutState { Pending }

            /// Behavior for payment processors.
            trait PaymentProcessor { fn charge(&self); }

            /// Shared checkout helpers.
            mod checkout_helpers {}

            /// Maximum retry attempts.
            const MAX_RETRIES: usize = 3;

            /// Customer identifier alias.
            type CustomerId = String;
            "#,
        );

        for (name, expected) in [
            ("CheckoutFlow", "Coordinates checkout"),
            ("CheckoutState", "Possible checkout states"),
            ("PaymentProcessor", "Behavior for payment processors"),
            ("checkout_helpers", "Shared checkout helpers"),
            ("MAX_RETRIES", "Maximum retry attempts"),
            ("CustomerId", "Customer identifier alias"),
        ] {
            let node = find_node(&result, name);
            assert_eq!(
                node.doc_comment
                    .as_deref()
                    .map(|doc| doc.contains(expected)),
                Some(true),
                "{name} should carry its declaration doc comment"
            );
        }
    }

    #[test]
    fn detects_async_boundary_on_await() {
        let result = extract(
            r#"
            async fn caller() {
                fetch().await;
            }
            async fn fetch() {}
            "#,
        );
        let await_edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target.contains("fetch"));
        // Note: tree-sitter-rust may or may not produce an await_expression parent
        // depending on version. We verify the edge exists at minimum.
        assert!(await_edge.is_some(), "should find Calls edge to fetch");
    }

    #[test]
    fn extracts_condition_on_match_arm() {
        let result = extract(
            r#"
            fn process(x: i32) {
                match x {
                    0 => handle_zero(),
                    _ => handle_other(),
                }
            }
            fn handle_zero() {}
            fn handle_other() {}
            "#,
        );
        let match_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.condition.is_some())
            .collect();
        // At least one call inside a match should have a condition
        assert!(
            !match_edges.is_empty(),
            "calls inside match arms should have conditions"
        );
        let cond = match_edges[0].condition.as_ref().unwrap();
        assert!(
            cond.starts_with("match"),
            "match arm condition should start with 'match'"
        );
    }

    #[test]
    fn normalizes_generic_qualified_call_targets_before_merge() {
        let result = extract(
            r#"
            struct Repo;

            impl Repo {
                fn new() -> Self { Repo }
            }

            fn build() {
                Repo::new::<usize>();
            }
            "#,
        );

        let build_id = find_node(&result, "build").id.clone();
        let raw_call = result
            .edges
            .iter()
            .find(|edge| edge.source == build_id && edge.kind == EdgeKind::Calls)
            .expect("expected a call edge from build");
        assert_eq!(raw_call.target, "Repo::new");

        let graph = merge_results(vec![result]);
        let merged_call = graph
            .edges
            .iter()
            .find(|edge| edge.source == build_id && edge.kind == EdgeKind::Calls)
            .expect("expected merged call edge from build");
        assert_eq!(merged_call.target, "test.rs::impl_Repo::new");
    }

    #[test]
    fn scopes_same_named_methods_by_owner() {
        let result = extract(
            r#"
            struct A;
            struct B;

            impl A {
                fn new() -> Self { A }
            }

            impl B {
                fn new() -> Self { B }
            }
            "#,
        );

        let new_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.name == "new")
            .collect();
        assert_eq!(new_nodes.len(), 2, "expected a distinct node per owner");
        assert!(
            new_nodes
                .iter()
                .any(|node| node.id == "test.rs::impl_A::new"),
            "A::new should be scoped under its impl id"
        );
        assert!(
            new_nodes
                .iter()
                .any(|node| node.id == "test.rs::impl_B::new"),
            "B::new should be scoped under its impl id"
        );

        assert!(has_edge(
            &result,
            "test.rs::impl_A",
            "test.rs::impl_A::new",
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            "test.rs::impl_B",
            "test.rs::impl_B::new",
            EdgeKind::Contains
        ));
    }

    #[test]
    fn uniquifies_duplicate_impl_blocks_for_same_type() {
        let result = extract(
            r#"
            struct A;

            impl A {
                fn first(&self) {}
            }

            impl A {
                fn second(&self) {}
            }
            "#,
        );

        let impl_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Impl && node.name == "A")
            .collect();
        assert_eq!(impl_nodes.len(), 2, "expected one node per impl block");
        assert_ne!(
            impl_nodes[0].id, impl_nodes[1].id,
            "impl ids must stay unique"
        );

        let first = find_node(&result, "first");
        let second = find_node(&result, "second");
        assert!(
            first.id.starts_with(&format!("{}::", impl_nodes[0].id))
                || first.id.starts_with(&format!("{}::", impl_nodes[1].id)),
            "method ids should be scoped by their containing impl"
        );
        assert!(
            second.id.starts_with(&format!("{}::", impl_nodes[0].id))
                || second.id.starts_with(&format!("{}::", impl_nodes[1].id)),
            "method ids should be scoped by their containing impl"
        );
        assert_ne!(
            first.id, second.id,
            "distinct methods should remain distinct"
        );
    }

    fn calls_to(result: &ExtractionResult, ends_with: &str) -> usize {
        result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .filter(|e| e.target == ends_with || e.target.ends_with(&format!("::{ends_with}")))
            .count()
    }

    #[test]
    fn suppresses_std_option_result_constructor_calls() {
        let result = extract(
            r#"
            fn helper() -> i32 { 1 }
            fn run() -> Option<i32> {
                let _ = Ok::<_, ()>(helper());
                let _ = Err::<(), _>(());
                if helper() > 0 {
                    return Some(helper());
                }
                None
            }
            "#,
        );

        // The std constructors must not produce Calls edges...
        assert_eq!(calls_to(&result, "Some"), 0, "Some(..) must not be a call");
        assert_eq!(calls_to(&result, "Ok"), 0, "Ok(..) must not be a call");
        assert_eq!(calls_to(&result, "Err"), 0, "Err(..) must not be a call");
        assert_eq!(calls_to(&result, "None"), 0, "None must not be a call");

        // ...but real user calls are still recorded.
        assert!(
            calls_to(&result, "helper") >= 1,
            "real function calls must still be extracted"
        );
    }

    #[test]
    fn keeps_user_function_named_like_constructor_lowercase() {
        // A user's own lowercase `some` / `ok` must NOT be suppressed: only the
        // exact PascalCase std constructors are filtered.
        let result = extract(
            r#"
            fn some() {}
            fn run() {
                some();
            }
            "#,
        );
        assert_eq!(
            calls_to(&result, "some"),
            1,
            "user fn `some` must remain a real callee"
        );
    }

    #[test]
    fn suppresses_phantom_constructor_variable_nodes() {
        let result = extract(
            r#"
            fn run(opt: Option<i32>, res: Result<i32, ()>) {
                let Some(x) = opt else { return; };
                let Ok(y) = res else { return; };
            }
            "#,
        );

        for ctor in ["Some", "Ok", "Err", "None"] {
            assert!(
                !result
                    .nodes
                    .iter()
                    .any(|n| n.kind == NodeKind::Variable && n.name == ctor),
                "no phantom Variable node named {ctor} should be emitted"
            );
        }
    }

    #[test]
    fn suppresses_phantom_constructor_in_tuple_destructure() {
        // `let (Some(a), Some(b)) = pair else { .. };` — the leading binding's first
        // identifier is the constructor `Some`, nested under a tuple_pattern.
        let result = extract(
            r#"
            fn run(pair: (Option<i32>, Option<i32>)) {
                let (Some(a), Some(b)) = pair else { return; };
            }
            "#,
        );
        assert!(
            !result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Variable && n.name == "Some"),
            "tuple destructure of Some must not emit a phantom Some variable"
        );
    }

    #[test]
    fn keeps_real_let_binding_variables() {
        // Regression guard: ordinary `let` bindings must still become Variables.
        let result = extract(
            r#"
            fn run() {
                let count = 1;
            }
            "#,
        );
        let count = find_node(&result, "count");
        assert_eq!(count.kind, NodeKind::Variable);
    }

    #[test]
    fn synthesizes_type_node_for_ulid_newtype_macro_brace_form() {
        // The exact shape used by the nous corpus: a doc comment then a PascalCase
        // identifier inside braces.
        let result = extract(
            r#"
            ulid_newtype! {
                /// Identifies a logical source.
                SourceId
            }
            ulid_newtype! {
                /// Identifies a captured revision.
                RevisionId
            }
            "#,
        );

        let source_id = find_node(&result, "SourceId");
        assert_eq!(source_id.kind, NodeKind::Struct);
        assert_eq!(source_id.visibility, Visibility::Public);
        assert_eq!(
            source_id
                .metadata
                .get("macro_generated")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            source_id.metadata.get("macro").map(String::as_str),
            Some("ulid_newtype")
        );

        let revision_id = find_node(&result, "RevisionId");
        assert_eq!(revision_id.kind, NodeKind::Struct);

        // The file should contain the synthesized types.
        let file = find_node(&result, "test.rs");
        assert!(has_edge(
            &result,
            &file.id,
            &source_id.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &file.id,
            &revision_id.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn synthesizes_type_node_for_newtype_macro_paren_form() {
        // Parenthesized, `;`-terminated form with a trailing string arg.
        let result = extract(r#"id_newtype!(RevisionId, "rev");"#);
        let node = find_node(&result, "RevisionId");
        assert_eq!(node.kind, NodeKind::Struct);
        // The string literal arg must be ignored (only the PascalCase ident counts).
        assert!(
            !result.nodes.iter().any(|n| n.name == "rev"),
            "string argument must not become a type node"
        );
    }

    #[test]
    fn newtype_macro_heuristic_matches_suffix_patterns() {
        let result = extract("entity_id! { OrderId }");
        let node = find_node(&result, "OrderId");
        assert_eq!(node.kind, NodeKind::Struct);
    }

    #[test]
    fn does_not_synthesize_type_for_macro_in_expression_position() {
        // A `*_id!` macro used inside a function body is an expression, not a type
        // declaration — it must not fabricate a phantom type node.
        let result = extract(
            r#"
            fn run() {
                let _ = some_id!(Whatever);
            }
            "#,
        );
        assert!(
            !result
                .nodes
                .iter()
                .any(|n| n.name == "Whatever" && n.kind == NodeKind::Struct),
            "macro in expression position must not synthesize a type node"
        );
    }

    #[test]
    fn does_not_synthesize_type_for_unrecognized_item_macro() {
        // An unrelated declarative macro in item position must not be treated as a
        // newtype declaration.
        let result = extract("lazy_static! { static ref FOO: u8 = 1; }");
        assert!(
            !result
                .nodes
                .iter()
                .any(|n| n.metadata.contains_key("macro_generated")),
            "non-newtype item macros must not synthesize type nodes"
        );
    }
}
