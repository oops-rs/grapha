use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;
use tree_sitter::{Parser, Tree};

use grapha_core::graph::{Edge, EdgeKind, Node, NodeKind, NodeRole, Visibility};
use grapha_core::{ExtractionResult, LanguageExtractor};

use super::common::*;
use super::swiftui::extract_swiftui_declaration_structure;

thread_local! {
    static SWIFT_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_swift::LANGUAGE.into()).expect("failed to load Swift grammar");
        p
    });
}

/// Parse Swift source once. Reuse the tree across enrichment passes.
pub fn parse_swift(source: &[u8]) -> anyhow::Result<Tree> {
    SWIFT_PARSER.with_borrow_mut(|parser| {
        parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse Swift source"))
    })
}

pub struct SwiftExtractor;

impl LanguageExtractor for SwiftExtractor {
    fn extract(&self, source: &[u8], file_path: &Path) -> anyhow::Result<ExtractionResult> {
        let tree = parse_swift(source)?;

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
        extract_swift_framework_nodes(&mut result, source, &file_str);
        emit_file_contains_edges(&mut result, &file_str, &file_node_id);

        Ok(result)
    }
}

pub fn enrich_codegraph_compat_with_tree(
    source: &[u8],
    file_path: &Path,
    tree: &Tree,
    result: &mut ExtractionResult,
) -> anyhow::Result<()> {
    let file_str = file_path.to_string_lossy().to_string();
    let file_node_id = format!("file:{file_str}");

    if !result.nodes.iter().any(|node| node.id == file_node_id) {
        push_file_node(
            result,
            tree.root_node(),
            file_path,
            &file_str,
            &file_node_id,
        );
    }
    extract_import_nodes_from_tree(tree.root_node(), source, &file_str, result);
    extract_swift_framework_nodes(result, source, &file_str);
    emit_file_contains_edges(result, &file_str, &file_node_id);

    Ok(())
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
    let mut metadata = HashMap::new();
    metadata.insert("language".to_string(), "swift".to_string());

    result.nodes.push(Node {
        id: file_node_id.to_string(),
        kind: NodeKind::File,
        name,
        file: file.into(),
        span: make_span(root),
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
            provenance: edge_provenance(file, span, file_node_id),
            repo: None,
        });
    }
}

fn extract_swift_framework_nodes(result: &mut ExtractionResult, source: &[u8], file: &str) {
    let Ok(content) = std::str::from_utf8(source) else {
        return;
    };

    let view_re = Regex::new(r#"struct\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*View"#)
        .expect("SwiftUI component regex should compile");
    for capture in view_re.captures_iter(content) {
        let Some(full_match) = capture.get(0) else {
            continue;
        };
        let Some(name) = capture.get(1).map(|m| m.as_str()) else {
            continue;
        };
        push_framework_node(
            result,
            file,
            source,
            FrameworkNodeSpec {
                kind: NodeKind::Component,
                id_prefix: "component",
                name,
                start_byte: full_match.start(),
                end_byte: full_match.end(),
                metadata: framework_metadata("component", None, None),
            },
        );
    }

    let route_re = Regex::new(r#"\.(get|post|put|patch|delete)\s*\(\s*["']([^"']+)["']"#)
        .expect("Swift route regex should compile");
    for capture in route_re.captures_iter(content) {
        let Some(full_match) = capture.get(0) else {
            continue;
        };
        let method = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let path = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
        push_swift_route_node(
            result,
            file,
            source,
            method,
            path,
            full_match.start(),
            full_match.end(),
        );
    }

    let grouped_re = Regex::new(
        r#"\.grouped\s*\(\s*["']([^"']+)["']\s*\)\s*\.(get|post|put|patch|delete)\s*\(\s*["']([^"']+)["']"#,
    )
    .expect("Swift grouped route regex should compile");
    for capture in grouped_re.captures_iter(content) {
        let Some(full_match) = capture.get(0) else {
            continue;
        };
        let prefix = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let method = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
        let path = capture.get(3).map(|m| m.as_str()).unwrap_or_default();
        let full_path = format!("{}/{}", prefix.trim_matches('/'), path.trim_matches('/'));
        push_swift_route_node(
            result,
            file,
            source,
            method,
            &format!("/{full_path}"),
            full_match.start(),
            full_match.end(),
        );
    }
}

fn extract_import_nodes_from_tree(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    result: &mut ExtractionResult,
) {
    if node.kind() == "import_declaration" {
        extract_import(node, source, file, result);
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_import_nodes_from_tree(child, source, file, result);
    }
}

fn push_swift_route_node(
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
    push_framework_node(
        result,
        file,
        source,
        FrameworkNodeSpec {
            kind: NodeKind::Route,
            id_prefix: "route",
            name: &name,
            start_byte,
            end_byte,
            metadata: framework_metadata("route", Some(&method), Some(path)),
        },
    );
}

struct FrameworkNodeSpec<'a> {
    kind: NodeKind,
    id_prefix: &'a str,
    name: &'a str,
    start_byte: usize,
    end_byte: usize,
    metadata: HashMap<String, String>,
}

fn push_framework_node(
    result: &mut ExtractionResult,
    file: &str,
    source: &[u8],
    spec: FrameworkNodeSpec<'_>,
) {
    let span = span_from_byte_range(source, spec.start_byte, spec.end_byte);
    let line = span.start[0] + 1;
    let id = format!("{}:{file}:{}:{line}", spec.id_prefix, spec.name);
    if result.nodes.iter().any(|node| node.id == id) {
        return;
    }
    let mut metadata = spec.metadata;
    metadata.insert("language".to_string(), "swift".to_string());
    result.nodes.push(Node {
        id,
        kind: spec.kind,
        name: spec.name.to_string(),
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

fn framework_metadata(
    kind: &str,
    method: Option<&str>,
    path: Option<&str>,
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert("framework.kind".to_string(), kind.to_string());
    if let Some(method) = method {
        metadata.insert("route.method".to_string(), method.to_string());
    }
    if let Some(path) = path {
        metadata.insert("route.path".to_string(), path.to_string());
    }
    metadata
}

fn span_from_byte_range(
    source: &[u8],
    start_byte: usize,
    end_byte: usize,
) -> grapha_core::graph::Span {
    grapha_core::graph::Span {
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

fn walk_node(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    match node.kind() {
        "class_declaration" => {
            let declaration_type = detect_class_declaration_type(node);
            match declaration_type {
                ClassDeclarationType::Struct => {
                    extract_struct_or_class(
                        node,
                        source,
                        file,
                        module_path,
                        parent_id,
                        NodeKind::Struct,
                        result,
                    );
                }
                ClassDeclarationType::Class => {
                    extract_struct_or_class(
                        node,
                        source,
                        file,
                        module_path,
                        parent_id,
                        NodeKind::Class,
                        result,
                    );
                }
                ClassDeclarationType::Enum => {
                    extract_enum(node, source, file, module_path, parent_id, result);
                }
                ClassDeclarationType::Extension => {
                    extract_extension(node, source, file, module_path, parent_id, result);
                }
            }
        }
        "protocol_declaration" => {
            extract_protocol(node, source, file, module_path, parent_id, result);
        }
        "function_declaration" | "init_declaration" | "deinit_declaration" => {
            extract_function(node, source, file, module_path, parent_id, result);
        }
        "protocol_function_declaration" => {
            extract_function(node, source, file, module_path, parent_id, result);
        }
        "property_declaration" => {
            extract_property(node, source, file, module_path, parent_id, result);
        }
        "typealias_declaration" => {
            extract_typealias(node, source, file, module_path, parent_id, result);
        }
        "import_declaration" => {
            extract_import(node, source, file, result);
        }
        _ => {
            walk_children(node, source, file, module_path, parent_id, result);
        }
    }
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

fn emit_contains_edge(
    parent_id: Option<&str>,
    child_id: &str,
    file: &str,
    edge_node: tree_sitter::Node,
    result: &mut ExtractionResult,
) {
    if let Some(pid) = parent_id {
        result.edges.push(Edge {
            source: pid.to_string(),
            target: child_id.to_string(),
            kind: EdgeKind::Contains,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: node_edge_provenance(file, edge_node, pid),
            repo: None,
        });
    }
}

fn swift_declaration_metadata(node: tree_sitter::Node, source: &[u8]) -> HashMap<String, String> {
    let Ok(raw) = node.utf8_text(source) else {
        return HashMap::new();
    };
    let signature = raw.split('{').next().unwrap_or(raw);
    let tokens = signature
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    let mut metadata = HashMap::new();
    if tokens.contains(&"async") {
        metadata.insert("async".to_string(), "true".to_string());
    }
    if tokens.contains(&"static")
        || (matches!(node.kind(), "function_declaration" | "property_declaration")
            && tokens.contains(&"class"))
    {
        metadata.insert("static".to_string(), "true".to_string());
    }
    metadata
}

/// Extract struct or class declaration.
fn extract_struct_or_class(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    kind: NodeKind,
    result: &mut ExtractionResult,
) {
    let Some(name) = type_identifier_text(node, source) else {
        return;
    };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    // Extract inheritance/conformance edges
    extract_inheritance_edges(node, source, file, module_path, &id, result);

    // Detect entry point: @main attribute
    let has_main_attr = has_swift_attribute(node, source, "main");
    let role = if has_main_attr {
        Some(NodeRole::EntryPoint)
    } else {
        None
    };

    // Detect conformances for body-level entry point marking
    let conformances = collect_inheritance_names(node, source);
    let conforms_to_view = conformances.iter().any(|c| c == "View" || c == "App");
    let is_observable = conformances.iter().any(|c| c == "ObservableObject")
        || has_swift_attribute(node, source, "Observable");

    result.nodes.push(Node {
        id: id.clone(),
        kind,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role,
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });

    // Walk the class_body for nested declarations
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "class_body" {
            walk_children_with_hints(
                child,
                source,
                file,
                module_path,
                Some(&id),
                conforms_to_view,
                is_observable,
                result,
            );
        }
    }
}

/// Walk children with hints about parent conformances for entry point detection.
#[allow(clippy::too_many_arguments)]
fn walk_children_with_hints(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    parent_conforms_to_view: bool,
    parent_is_observable: bool,
    result: &mut ExtractionResult,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "property_declaration" && parent_conforms_to_view {
            // Check if this is the `body` property
            if let Some(prop_name) = find_pattern_name(child, source)
                && prop_name == "body"
            {
                extract_property_as_entry_point(
                    child,
                    source,
                    file,
                    module_path,
                    parent_id,
                    result,
                );
                continue;
            }
        }
        if child.kind() == "function_declaration" && parent_is_observable {
            // Mark public methods as entry points
            extract_function_with_entry_hint(
                child,
                source,
                file,
                module_path,
                parent_id,
                true,
                result,
            );
            continue;
        }
        walk_node(child, source, file, module_path, parent_id, result);
    }
}

/// Extract a property and mark it as an entry point (e.g., View.body).
fn extract_property_as_entry_point(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    let name = find_pattern_name(node, source);
    let Some(name) = name else { return };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Property,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role: Some(NodeRole::EntryPoint),
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });

    // Scan property body for calls (same as extract_property)
    extract_calls(node, source, file, module_path, &id, result);
    // Always run regex fallback to catch calls inside closures/ViewBuilder
    // bodies that tree-sitter doesn't parse as call_expression nodes.
    if let Ok(text) = node.utf8_text(source) {
        extract_calls_from_text(text, node, file, module_path, &id, result);
    }
    extract_swiftui_declaration_structure(node, source, file, module_path, &id, result);
}

/// Extract a function with an optional entry point hint from parent (Observable).
fn extract_function_with_entry_hint(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    observable_parent: bool,
    result: &mut ExtractionResult,
) {
    // init/deinit declarations don't have a simple_identifier name
    let name = if node.kind() == "init_declaration" {
        "init".to_string()
    } else if node.kind() == "deinit_declaration" {
        "deinit".to_string()
    } else {
        let Some(n) = simple_identifier_text(node, source) else {
            return;
        };
        n
    };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);
    let signature = extract_swift_signature(node, source);
    let doc_comment = extract_swift_doc_comment(node, source);

    let role = if observable_parent
        && (visibility == Visibility::Public || visibility == Visibility::Crate)
    {
        Some(NodeRole::EntryPoint)
    } else {
        None
    };

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Function,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role,
        signature,
        doc_comment,
        module: None,
        snippet: None,
        repo: None,
    });

    // Walk function body for call expressions.
    // init_declaration uses "class_body" or direct children, not "function_body"
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if matches!(child.kind(), "function_body" | "class_body") {
            extract_calls(child, source, file, module_path, &id, result);
        }
    }
    // Fallback: if no function_body found (e.g., init), scan all children
    // that aren't parameter lists or modifiers
    let has_body = {
        let mut c = node.walk();
        node.named_children(&mut c)
            .any(|ch| matches!(ch.kind(), "function_body" | "class_body"))
    };
    if !has_body {
        let mut c = node.walk();
        for child in node.named_children(&mut c) {
            if !matches!(
                child.kind(),
                "modifiers" | "parameter" | "type_annotation" | "attribute" | "where_clause"
            ) {
                extract_calls(child, source, file, module_path, &id, result);
            }
        }
    }
}

/// Extract enum declaration with case variants.
fn extract_enum(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    let Some(name) = type_identifier_text(node, source) else {
        return;
    };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Enum,
        name: name.clone(),
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role: None,
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });

    // Extract enum entries from enum_class_body
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "enum_class_body" {
            extract_enum_entries(child, source, file, module_path, &id, result);
            walk_children(child, source, file, module_path, Some(&id), result);
        }
    }
}

/// Extract enum_entry children from an enum body.
fn extract_enum_entries(
    body: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: &str,
    result: &mut ExtractionResult,
) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "enum_entry"
            && let Some(case_name) = simple_identifier_text(child, source)
        {
            let id = make_decl_id(file, module_path, Some(parent_id), &case_name);

            result.edges.push(Edge {
                source: parent_id.to_string(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: node_edge_provenance(file, child, parent_id),
                repo: None,
            });

            result.nodes.push(Node {
                id,
                kind: NodeKind::Variant,
                name: case_name,
                file: file.into(),
                span: make_span(child),
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: extract_swift_doc_comment(child, source),
                module: None,
                snippet: None,
                repo: None,
            });
        }
    }
}

/// Extract extension declaration.
fn extract_extension(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    // Extension uses user_type > type_identifier for the extended type name
    let name = find_user_type_name(node, source).unwrap_or_else(|| "Unknown".to_string());
    let ext_name = format!("ext_{}", name);
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &ext_name),
        node,
    );

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Extension,
        name,
        file: file.into(),
        span: make_span(node),
        visibility: Visibility::Crate,
        metadata: swift_declaration_metadata(node, source),
        role: None,
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });

    // Walk the class_body for nested declarations
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "class_body" {
            walk_children(child, source, file, module_path, Some(&id), result);
        }
    }
}

/// Find the type name from a `user_type > type_identifier` child.
fn extract_protocol(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    let Some(name) = type_identifier_text(node, source) else {
        return;
    };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Protocol,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role: None,
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });

    // Walk the protocol_body for method declarations
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "protocol_body" {
            walk_children(child, source, file, module_path, Some(&id), result);
        }
    }
}

/// Extract function declaration (including init/deinit).
fn extract_function(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    let name = if node.kind() == "init_declaration" {
        "init".to_string()
    } else if node.kind() == "deinit_declaration" {
        "deinit".to_string()
    } else {
        let Some(n) = simple_identifier_text(node, source) else {
            return;
        };
        n
    };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);
    let signature = extract_swift_signature(node, source);
    let doc_comment = extract_swift_doc_comment(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Function,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role: None,
        signature,
        doc_comment,
        module: None,
        snippet: None,
        repo: None,
    });

    // Walk function body for call expressions.
    // init_declaration may not have "function_body" — scan all non-parameter children as fallback.
    let mut cursor = node.walk();
    let mut found_body = false;
    for child in node.named_children(&mut cursor) {
        if child.kind() == "function_body" {
            extract_calls(child, source, file, module_path, &id, result);
            found_body = true;
        }
    }
    if !found_body {
        let mut c = node.walk();
        for child in node.named_children(&mut c) {
            if !matches!(
                child.kind(),
                "modifiers"
                    | "parameter"
                    | "type_annotation"
                    | "attribute"
                    | "where_clause"
                    | "simple_identifier"
                    | "type_identifier"
            ) {
                extract_calls(child, source, file, module_path, &id, result);
            }
        }
    }

    if declaration_returns_swiftui_view(node, source) {
        extract_swiftui_declaration_structure(node, source, file, module_path, &id, result);
    }
}

/// Extract property declaration.
fn extract_property(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    // Property name is in pattern > simple_identifier
    let name = find_pattern_name(node, source);
    let Some(name) = name else { return };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Property,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role: None,
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });

    // Scan property body for calls via tree-sitter AST
    extract_calls(node, source, file, module_path, &id, result);

    // Fallback: if no calls edges were found via AST, scan the property's source
    // text for function call patterns using regex. This handles cases where
    // tree-sitter-swift doesn't produce call_expression nodes (e.g., SwiftUI
    // View body with result builders).
    let calls_before = result
        .edges
        .iter()
        .filter(|e| e.source == id && e.kind == EdgeKind::Calls)
        .count();
    if calls_before == 0
        && let Ok(text) = node.utf8_text(source)
    {
        extract_calls_from_text(text, node, file, module_path, &id, result);
    }

    if declaration_returns_swiftui_view(node, source) {
        extract_swiftui_declaration_structure(node, source, file, module_path, &id, result);
    }
}

/// Extract function calls from raw source text using regex.
/// Fallback for when tree-sitter doesn't produce call_expression nodes
/// (e.g., SwiftUI View body with result builders).
fn extract_calls_from_text(
    text: &str,
    node: tree_sitter::Node,
    file: &str,
    module_path: &[String],
    caller_id: &str,
    result: &mut ExtractionResult,
) {
    // Match patterns like: identifier( or .identifier(
    // But skip common keywords and type annotations
    let call_re = regex::Regex::new(r"(?:^|[.\s({,])([a-z][a-zA-Z0-9]*)\s*\(").unwrap();
    let skip_names: std::collections::HashSet<&str> = [
        "if", "for", "while", "switch", "guard", "return", "let", "var", "case", "some", "in",
        "as", "is", "try", "await", "throw", "catch", "where",
    ]
    .into_iter()
    .collect();

    let mut seen = std::collections::HashSet::new();
    for cap in call_re.captures_iter(text) {
        let fn_name = cap.get(1).unwrap().as_str();
        if skip_names.contains(fn_name) || !seen.insert(fn_name.to_string()) {
            continue;
        }
        let target_id = make_id(file, module_path, fn_name);
        let span = cap
            .get(1)
            .and_then(|capture| span_from_text_range(node, text, capture.start(), capture.end()))
            .unwrap_or_else(|| make_span(node));
        result.edges.push(Edge {
            source: caller_id.to_string(),
            target: target_id,
            kind: EdgeKind::Calls,
            confidence: 0.5, // lower confidence for regex-based extraction
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: edge_provenance(file, span, caller_id),
            repo: None,
        });
    }

    // Also match property access chains like AppContext.gift.activityGiftConfigs
    let nav_re = regex::Regex::new(r"[A-Za-z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)+").unwrap();
    for mat in nav_re.find_iter(text) {
        let chain = mat.as_str();
        let parts: Vec<&str> = chain.split('.').collect();
        if parts.len() >= 2 {
            let last = *parts.last().unwrap();
            if skip_names.contains(last) || seen.contains(last) {
                continue;
            }
            seen.insert(last.to_string());
            let prefix = parts[..parts.len() - 1].join(".");
            let target_id = make_id(file, module_path, last);
            let span = span_from_text_range(node, text, mat.start(), mat.end())
                .unwrap_or_else(|| make_span(node));
            result.edges.push(Edge {
                source: caller_id.to_string(),
                target: target_id,
                kind: EdgeKind::Reads,
                confidence: 0.5,
                direction: None,
                operation: Some(prefix),
                condition: None,
                async_boundary: None,
                provenance: edge_provenance(file, span, caller_id),
                repo: None,
            });
        }
    }
}

/// Find the name from a `pattern > simple_identifier` child.
fn extract_typealias(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    result: &mut ExtractionResult,
) {
    let Some(name) = type_identifier_text(node, source) else {
        return;
    };
    let id = unique_decl_id(
        result,
        make_decl_id(file, module_path, parent_id, &name),
        node,
    );
    let visibility = extract_visibility(node, source);

    emit_contains_edge(parent_id, &id, file, node, result);

    result.nodes.push(Node {
        id,
        kind: NodeKind::TypeAlias,
        name,
        file: file.into(),
        span: make_span(node),
        visibility,
        metadata: swift_declaration_metadata(node, source),
        role: None,
        signature: None,
        doc_comment: extract_swift_doc_comment(node, source),
        module: None,
        snippet: None,
        repo: None,
    });
}

/// Extract import declaration into an Import struct.
fn extract_import(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    result: &mut ExtractionResult,
) {
    let signature = node
        .utf8_text(source)
        .unwrap_or_default()
        .trim()
        .to_string();
    // The import path is in the identifier > simple_identifier child
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(path_text) = child.utf8_text(source)
        {
            let path = path_text.to_string();
            if !push_import_node(result, file, node, &path, &signature) {
                continue;
            }

            if !result.imports.iter().any(|import| {
                import.path == path && import.kind == grapha_core::resolve::ImportKind::Module
            }) {
                result.imports.push(grapha_core::resolve::Import {
                    path: path.clone(),
                    symbols: vec![],
                    kind: grapha_core::resolve::ImportKind::Module,
                });
            }

            result.edges.push(Edge {
                source: file.to_string(),
                target: format!("import {}", path),
                kind: EdgeKind::Uses,
                confidence: 0.7,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: node_edge_provenance(file, node, file),
                repo: None,
            });
        }
    }
}

fn push_import_node(
    result: &mut ExtractionResult,
    file: &str,
    node: tree_sitter::Node,
    path: &str,
    signature: &str,
) -> bool {
    if result.nodes.iter().any(|node| {
        node.kind == NodeKind::Import && node.file == Path::new(file) && node.name == path
    }) {
        return false;
    }
    let id = unique_decl_id(result, make_id(file, &[], &format!("import:{path}")), node);
    let mut metadata = HashMap::new();
    metadata.insert("language".to_string(), "swift".to_string());

    result.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Import,
        name: path.to_string(),
        file: file.into(),
        span: make_span(node),
        visibility: Visibility::Private,
        metadata,
        role: None,
        signature: Some(signature.to_string()),
        doc_comment: None,
        module: None,
        snippet: None,
        repo: None,
    });
    let file_node_id = format!("file:{file}");
    result.edges.push(Edge {
        source: file_node_id.clone(),
        target: id,
        kind: EdgeKind::Imports,
        confidence: 0.9,
        direction: None,
        operation: None,
        condition: None,
        async_boundary: None,
        provenance: node_edge_provenance(file, node, &file_node_id),
        repo: None,
    });
    true
}

/// Extract function signature (text up to opening `{`).
fn extract_swift_signature(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else {
        text.trim()
    };
    if sig.is_empty() {
        None
    } else {
        Some(sig.to_string())
    }
}

/// Enrich an existing `ExtractionResult` (e.g. from the index store) with doc
/// comments extracted via tree-sitter.  The index store does not provide doc
/// comments, so we do a lightweight tree-sitter parse and match nodes by
/// `(name, start_line)`.
///
/// Index store lines are **1-based**; tree-sitter rows are **0-based**, so we
/// compare with `row + 1`.
#[cfg(test)]
pub fn enrich_doc_comments(source: &[u8], result: &mut ExtractionResult) -> anyhow::Result<()> {
    let tree = parse_swift(source)?;
    enrich_doc_comments_with_tree(source, &tree, result)
}

pub fn enrich_doc_comments_with_tree(
    source: &[u8],
    tree: &Tree,
    result: &mut ExtractionResult,
) -> anyhow::Result<()> {
    // Collect (name, 1-based line) → doc_comment from tree-sitter AST.
    let mut doc_map: HashMap<(String, usize), String> = HashMap::new();
    collect_doc_comments(tree.root_node(), source, &mut doc_map);

    // Patch nodes that are missing a doc_comment.
    // Index-store spans are 1-based, while tree-sitter/SwiftSyntax spans are
    // 0-based. Try the node's stored line first, then a 1-based adjustment.
    for node in &mut result.nodes {
        if node.doc_comment.is_some() {
            continue;
        }
        let line = node.span.start[0];
        let doc = [
            Some(line),
            line.checked_sub(1),
            Some(line + 1),
            Some(line + 2),
        ]
        .into_iter()
        .flatten()
        .find_map(|candidate_line| doc_map.remove(&(node.name.clone(), candidate_line)));
        if let Some(doc) = doc {
            node.doc_comment = Some(doc);
        }
    }

    Ok(())
}

/// Recursively walk the tree-sitter AST and collect doc comments for every
/// declaration that has one.  Results are keyed by `(name, 1-based line)`.
fn collect_doc_comments(
    node: tree_sitter::Node,
    source: &[u8],
    out: &mut HashMap<(String, usize), String>,
) {
    match node.kind() {
        "class_declaration"
        | "struct_declaration"
        | "enum_declaration"
        | "protocol_declaration" => {
            if let Some(name) = type_identifier_text(node, source)
                && let Some(doc) = extract_swift_doc_comment(node, source)
            {
                out.insert((name, node.start_position().row + 1), doc);
            }
        }
        "function_declaration"
        | "init_declaration"
        | "deinit_declaration"
        | "protocol_function_declaration" => {
            let name = if node.kind() == "init_declaration" {
                Some("init".to_string())
            } else if node.kind() == "deinit_declaration" {
                Some("deinit".to_string())
            } else {
                simple_identifier_text(node, source)
            };
            if let Some(name) = name
                && let Some(doc) = extract_swift_doc_comment(node, source)
            {
                out.insert((name, node.start_position().row + 1), doc);
            }
        }
        "property_declaration" => {
            if let Some(name) = find_pattern_name(node, source)
                && let Some(doc) = extract_swift_doc_comment(node, source)
            {
                out.insert((name, node.start_position().row + 1), doc);
            }
        }
        "typealias_declaration" => {
            if let Some(name) = type_identifier_text(node, source)
                && let Some(doc) = extract_swift_doc_comment(node, source)
            {
                out.insert((name, node.start_position().row + 1), doc);
            }
        }
        "enum_entry" => {
            if let Some(name) = simple_identifier_text(node, source)
                && let Some(doc) = extract_swift_doc_comment(node, source)
            {
                out.insert((name, node.start_position().row + 1), doc);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_doc_comments(child, source, out);
    }
}

/// Extract doc comments from previous sibling comment nodes.
fn detect_swift_async_boundary(node: tree_sitter::Node, source: &[u8]) -> Option<bool> {
    // Check if parent is await_expression
    if let Some(parent) = node.parent()
        && parent.kind() == "await_expression"
    {
        return Some(true);
    }
    // Check if inside a Task { } block
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_declaration" || parent.kind() == "closure_expression" {
            // Check if the closure is an argument to Task { } or DispatchQueue.async
            if let Some(gp) = parent.parent()
                && gp.kind() == "call_expression"
            {
                if let Some(fn_name) = simple_identifier_text(gp, source)
                    && fn_name == "Task"
                {
                    return Some(true);
                }
                if let Ok(text) = gp.utf8_text(source)
                    && text.contains("DispatchQueue")
                    && text.contains("async")
                {
                    return Some(true);
                }
            }
            break;
        }
        current = parent.parent();
    }
    None
}

/// Extract inheritance/conformance edges from `inheritance_specifier` children.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeDeclarationKind {
    Class,
    Protocol,
}

fn root_node(mut node: tree_sitter::Node) -> tree_sitter::Node {
    while let Some(parent) = node.parent() {
        node = parent;
    }
    node
}

fn collect_type_declaration_kinds(
    node: tree_sitter::Node,
    source: &[u8],
    out: &mut HashMap<String, TypeDeclarationKind>,
) {
    match node.kind() {
        "class_declaration" => {
            if matches!(
                detect_class_declaration_type(node),
                ClassDeclarationType::Class
            ) && let Some(name) = type_identifier_text(node, source)
            {
                out.insert(name, TypeDeclarationKind::Class);
            }
        }
        "protocol_declaration" => {
            if let Some(name) = type_identifier_text(node, source) {
                out.insert(name, TypeDeclarationKind::Protocol);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_declaration_kinds(child, source, out);
    }
}

fn is_known_external_protocol_name(name: &str) -> bool {
    matches!(
        name,
        "Actor"
            | "AnyObject"
            | "CaseIterable"
            | "Codable"
            | "Decodable"
            | "Encodable"
            | "Equatable"
            | "Error"
            | "Hashable"
            | "Identifiable"
            | "Observable"
            | "ObservableObject"
            | "RawRepresentable"
            | "Sendable"
            | "View"
    )
}

fn is_likely_external_class_name(name: &str) -> bool {
    name.starts_with("NS")
        || name.starts_with("UI")
        || name.ends_with("Controller")
        || name.ends_with("ViewController")
        || name.ends_with("View")
        || name.ends_with("Object")
        || name.ends_with("Responder")
        || name.ends_with("Window")
}

fn extract_inheritance_edges(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    type_id: &str,
    result: &mut ExtractionResult,
) {
    let owner_kind = node.kind();
    let mut declaration_kinds = HashMap::new();
    collect_type_declaration_kinds(root_node(node), source, &mut declaration_kinds);
    let mut inheritance_specifiers = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "inheritance_specifier" {
            continue;
        }

        let Some(inherited_name) =
            find_user_type_name(child, source).or_else(|| type_identifier_text(child, source))
        else {
            continue;
        };

        inheritance_specifiers.push((child, inherited_name));
    }

    let resolved_class_parent = if owner_kind == "class_declaration" {
        inheritance_specifiers
            .iter()
            .position(|(_, inherited_name)| {
                declaration_kinds.get(inherited_name) == Some(&TypeDeclarationKind::Class)
            })
    } else {
        None
    };
    let inferred_external_class_parent = if owner_kind == "class_declaration"
        && resolved_class_parent.is_none()
        && inheritance_specifiers.len() > 1
        && declaration_kinds.get(&inheritance_specifiers[0].1)
            != Some(&TypeDeclarationKind::Protocol)
        && !is_known_external_protocol_name(&inheritance_specifiers[0].1)
        && is_likely_external_class_name(&inheritance_specifiers[0].1)
        && inheritance_specifiers
            .iter()
            .skip(1)
            .any(|(_, inherited_name)| {
                declaration_kinds.get(inherited_name) == Some(&TypeDeclarationKind::Protocol)
            }) {
        Some(0)
    } else {
        None
    };
    let class_parent_index = resolved_class_parent.or(inferred_external_class_parent);

    for (index, (child, inherited_name)) in inheritance_specifiers.into_iter().enumerate() {
        let target_id = make_id(file, module_path, &inherited_name);
        let edge_kind = match owner_kind {
            "class_declaration" if class_parent_index == Some(index) => EdgeKind::Inherits,
            "class_declaration" => EdgeKind::Implements,
            "protocol_declaration" => EdgeKind::Inherits,
            _ => EdgeKind::Implements,
        };

        result.edges.push(Edge {
            source: type_id.to_string(),
            target: target_id,
            kind: edge_kind,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: node_edge_provenance(file, child, type_id),
            repo: None,
        });
    }
}

/// Extract the prefix of a navigation expression chain.
/// For `AppContext.gift.activityGiftConfigs`, returns `Some("AppContext.gift")`.
/// For `foo.bar`, returns `Some("foo")`.
fn extract_nav_prefix(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    // The first named child that isn't a navigation_suffix is the prefix expression
    let first_child = node
        .named_children(&mut cursor)
        .find(|c| c.kind() != "navigation_suffix")?;
    let text = first_child.utf8_text(source).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Recursively scan for `call_expression` and `navigation_expression` nodes,
/// emitting Calls edges for function calls and Reads edges for property accesses.
fn extract_calls(
    node: tree_sitter::Node,
    source: &[u8],
    file: &str,
    module_path: &[String],
    caller_id: &str,
    result: &mut ExtractionResult,
) {
    if node.kind() == "call_expression"
        && let Some(fn_name) = simple_identifier_text(node, source)
    {
        let target_id = make_id(file, module_path, &fn_name);
        let condition = find_enclosing_swift_condition(node, source);
        let async_boundary = detect_swift_async_boundary(node, source);
        result.edges.push(Edge {
            source: caller_id.to_string(),
            target: target_id,
            kind: EdgeKind::Calls,
            confidence: 0.8,
            direction: None,
            operation: None,
            condition,
            async_boundary,
            provenance: node_edge_provenance(file, node, caller_id),
            repo: None,
        });
    }

    // Generic constructor calls: `Type<Generic>(args)` produces a
    // `constructor_expression` node with the type name in `user_type > type_identifier`.
    if node.kind() == "constructor_expression" {
        let mut ctor_cursor = node.walk();
        let type_name = node
            .named_children(&mut ctor_cursor)
            .find(|c| c.kind() == "user_type")
            .and_then(|ut| type_identifier_text(ut, source));
        if let Some(name) = type_name {
            let target_id = make_id(file, module_path, &name);
            let condition = find_enclosing_swift_condition(node, source);
            let async_boundary = detect_swift_async_boundary(node, source);
            result.edges.push(Edge {
                source: caller_id.to_string(),
                target: target_id,
                kind: EdgeKind::Calls,
                confidence: 0.8,
                direction: None,
                operation: None,
                condition,
                async_boundary,
                provenance: node_edge_provenance(file, node, caller_id),
                repo: None,
            });
        }
    }

    // Property access: `foo.bar` generates a navigation_expression.
    // Emit a Reads edge so dependency queries can trace through the access
    // without polluting call lists with field-like symbols.
    // Skip if the parent is a call_expression (already handled above as the callee name).
    if node.kind() == "navigation_expression"
        && !matches!(node.parent().map(|p| p.kind()), Some("call_expression"))
    {
        // The accessed property name is the last navigation_suffix child
        let mut cursor = node.walk();
        if let Some(suffix) = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "navigation_suffix")
            .last()
            && let Some(name_node) = suffix.named_child(0)
            && let Ok(prop_name) = name_node.utf8_text(source)
            && !prop_name.is_empty()
        {
            let target_id = make_id(file, module_path, prop_name);
            let condition = find_enclosing_swift_condition(node, source);
            // Extract the prefix chain (e.g., "AppContext.gift" from "AppContext.gift.activityGiftConfigs")
            // to help the merge step disambiguate among multiple candidates.
            let prefix = extract_nav_prefix(node, source);
            result.edges.push(Edge {
                source: caller_id.to_string(),
                target: target_id,
                kind: EdgeKind::Reads,
                confidence: 0.6,
                direction: None,
                operation: prefix,
                condition,
                async_boundary: None,
                provenance: node_edge_provenance(file, node, caller_id),
                repo: None,
            });
        }
    }

    // Recurse into all children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_calls(child, source, file, module_path, caller_id, result);
    }
}
