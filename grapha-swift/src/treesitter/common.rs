use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use grapha_core::ExtractionResult;
use grapha_core::graph::{Edge, EdgeKind, EdgeProvenance, Node, NodeKind, Span, Visibility};

#[derive(Clone)]
pub(super) struct NodeSnapshot {
    pub(super) id: String,
    pub(super) kind: NodeKind,
    pub(super) name: String,
    pub(super) file: PathBuf,
    pub(super) span: Span,
}

impl From<&Node> for NodeSnapshot {
    fn from(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            kind: node.kind,
            name: node.name.clone(),
            file: node.file.clone(),
            span: node.span.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct EdgeDedupKey {
    source: String,
    target: String,
    kind: EdgeKind,
    operation: Option<String>,
}

impl From<&Edge> for EdgeDedupKey {
    fn from(edge: &Edge) -> Self {
        Self {
            source: edge.source.clone(),
            target: edge.target.clone(),
            kind: edge.kind,
            operation: edge.operation.clone(),
        }
    }
}

pub(super) struct EnrichmentContext<'a> {
    pub(super) result: &'a mut ExtractionResult,
    pub(super) node_snapshots: Vec<NodeSnapshot>,
    nodes_by_kind_name: HashMap<(NodeKind, String), Vec<usize>>,
    node_index_by_id: HashMap<String, usize>,
    candidate_owner_names: HashMap<String, Vec<String>>,
    existing_node_ids: HashSet<String>,
    existing_edge_keys: HashSet<EdgeDedupKey>,
}

impl<'a> EnrichmentContext<'a> {
    pub(super) fn new(result: &'a mut ExtractionResult) -> Self {
        let candidate_owner_names = candidate_owner_names(result);
        let existing_edge_keys = result.edges.iter().map(EdgeDedupKey::from).collect();
        let existing_node_ids = result.nodes.iter().map(|node| node.id.clone()).collect();
        let snapshots: Vec<(usize, NodeSnapshot)> = result
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| (idx, NodeSnapshot::from(node)))
            .collect();
        let mut context = Self {
            result,
            node_snapshots: Vec::with_capacity(snapshots.len()),
            nodes_by_kind_name: HashMap::new(),
            node_index_by_id: HashMap::new(),
            candidate_owner_names,
            existing_node_ids,
            existing_edge_keys,
        };
        for (result_index, snapshot) in snapshots {
            context.track_node(snapshot, result_index);
        }
        context
    }

    fn track_node(&mut self, snapshot: NodeSnapshot, result_index: usize) {
        let snapshot_index = self.node_snapshots.len();
        self.node_index_by_id
            .insert(snapshot.id.clone(), result_index);
        self.nodes_by_kind_name
            .entry((snapshot.kind, snapshot.name.clone()))
            .or_default()
            .push(snapshot_index);
        self.node_snapshots.push(snapshot);
    }

    pub(super) fn node_mut(&mut self, node_id: &str) -> Option<&mut Node> {
        let index = *self.node_index_by_id.get(node_id)?;
        self.result.nodes.get_mut(index)
    }

    pub(super) fn local_wrapper_id(&self, file_path: &Path, wrapper_name: &str) -> Option<String> {
        self.result
            .nodes
            .iter()
            .find(|node| {
                file_matches(&node.file, file_path)
                    && node.name == wrapper_name
                    && node.metadata.contains_key("l10n.wrapper.key")
            })
            .map(|node| node.id.clone())
    }

    pub(super) fn push_node(&mut self, node: Node) -> bool {
        if !self.existing_node_ids.insert(node.id.clone()) {
            return false;
        }
        let result_index = self.result.nodes.len();
        let snapshot = NodeSnapshot::from(&node);
        self.result.nodes.push(node);
        self.track_node(snapshot, result_index);
        true
    }

    pub(super) fn emit_edge(&mut self, edge: Edge) {
        let key = EdgeDedupKey::from(&edge);
        if self.existing_edge_keys.insert(key) {
            self.result.edges.push(edge);
        }
    }

    pub(super) fn matching_declaration_id(
        &self,
        file_path: &Path,
        decl_node: tree_sitter::Node,
        source: &[u8],
    ) -> Option<String> {
        let decl_line = decl_node.start_position().row;
        let decl_name = declaration_name(decl_node, source)?;
        let decl_kind = declaration_kind(decl_node)?;
        let owner_name = enclosing_owner_type_name(decl_node, source);
        let key = (decl_kind, decl_name.clone());
        let mut candidates = self
            .nodes_by_kind_name
            .get(&key)
            .into_iter()
            .flatten()
            .map(|snapshot_index| &self.node_snapshots[*snapshot_index])
            .filter(|node| file_matches(&node.file, file_path))
            .collect::<Vec<_>>();

        if candidates.is_empty() {
            let normalized_decl_name = normalized_declaration_match_name(&decl_name);
            candidates = self
                .node_snapshots
                .iter()
                .filter(|node| {
                    node.kind == decl_kind
                        && file_matches(&node.file, file_path)
                        && normalized_declaration_match_name(&node.name) == normalized_decl_name
                })
                .collect();
        }

        let line_matches = candidates
            .iter()
            .copied()
            .filter(|node| line_matches(node.span.start[0], decl_line))
            .collect::<Vec<_>>();
        if !line_matches.is_empty() {
            return line_matches
                .into_iter()
                .min_by_key(|node| node.span.start[0].abs_diff(decl_line))
                .map(|node| node.id.clone());
        }

        if let Some(owner_name) = owner_name {
            let owner_matches = candidates
                .iter()
                .copied()
                .filter(|node| {
                    self.candidate_owner_names
                        .get(&node.id)
                        .is_some_and(|owners| owners.iter().any(|owner| owner == &owner_name))
                })
                .collect::<Vec<_>>();
            if owner_matches.len() == 1 {
                return Some(owner_matches[0].id.clone());
            }
            if !owner_matches.is_empty() {
                return owner_matches
                    .into_iter()
                    .min_by_key(|node| node.span.start[0].abs_diff(decl_line))
                    .map(|node| node.id.clone());
            }
        }

        if candidates.len() == 1 {
            return Some(candidates[0].id.clone());
        }

        candidates
            .into_iter()
            .min_by_key(|node| node.span.start[0].abs_diff(decl_line))
            .map(|node| node.id.clone())
    }

    pub(super) fn matching_body_id(
        &self,
        file_path: &Path,
        body_node: tree_sitter::Node,
    ) -> Option<String> {
        let body_line = body_node.start_position().row;
        self.nodes_by_kind_name
            .get(&(NodeKind::Property, "body".to_string()))?
            .iter()
            .map(|snapshot_index| &self.node_snapshots[*snapshot_index])
            .filter(|node| {
                file_matches(&node.file, file_path) && line_matches(node.span.start[0], body_line)
            })
            .min_by_key(|node| node.span.start[0].abs_diff(body_line))
            .map(|node| node.id.clone())
    }
}

pub(super) fn make_id(file: &str, module_path: &[String], name: &str) -> String {
    if module_path.is_empty() {
        format!("{}::{}", file, name)
    } else {
        format!("{}::{}::{}", file, module_path.join("::"), name)
    }
}

/// Build a declaration/member ID scoped to its owning declaration when present.
pub(super) fn make_decl_id(
    file: &str,
    module_path: &[String],
    parent_id: Option<&str>,
    name: &str,
) -> String {
    parent_id
        .map(|pid| format!("{pid}::{name}"))
        .unwrap_or_else(|| make_id(file, module_path, name))
}

pub(super) fn unique_decl_id(
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

    let span = make_span(node);
    format!(
        "{proposed_id}@{}:{}:{}:{}",
        span.start[0], span.start[1], span.end[0], span.end[1]
    )
}

/// Extract the text of the first `simple_identifier` named child (used for function names).
pub(super) fn simple_identifier_text(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "simple_identifier" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Extract the `type_identifier` named child text (used for type names).
pub(super) fn type_identifier_text(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "type_identifier" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Determine the Swift declaration kind from a `class_declaration` node.
///
/// tree-sitter-swift uses `class_declaration` for struct, class, enum, and extension.
/// We distinguish them by the anonymous keyword child token.
pub(super) fn detect_class_declaration_type(node: tree_sitter::Node) -> ClassDeclarationType {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            match child.kind() {
                "struct" => return ClassDeclarationType::Struct,
                "class" => return ClassDeclarationType::Class,
                "enum" => return ClassDeclarationType::Enum,
                "extension" => return ClassDeclarationType::Extension,
                _ => {}
            }
        }
    }
    // Default to class if we can't determine
    ClassDeclarationType::Class
}

pub(super) enum ClassDeclarationType {
    Struct,
    Class,
    Enum,
    Extension,
}

/// Extract visibility from a Swift node by checking for a `modifiers` child
/// containing a `visibility_modifier`.
pub(super) fn extract_visibility(node: tree_sitter::Node, source: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.named_children(&mut mod_cursor) {
                if modifier.kind() == "visibility_modifier" {
                    let text = modifier.utf8_text(source).unwrap_or("");
                    if text == "public" || text == "open" {
                        return Visibility::Public;
                    } else if text == "private" || text == "fileprivate" {
                        return Visibility::Private;
                    }
                    // "internal" is default in Swift, maps to Crate
                    return Visibility::Crate;
                }
            }
        }
    }
    // Swift default is `internal`
    Visibility::Crate
}

pub(super) fn make_span(node: tree_sitter::Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span {
        start: [start.row, start.column],
        end: [end.row, end.column],
    }
}

pub(super) fn edge_provenance(file: &str, span: Span, symbol_id: &str) -> Vec<EdgeProvenance> {
    vec![EdgeProvenance {
        file: file.into(),
        span,
        symbol_id: symbol_id.to_string(),
    }]
}

pub(super) fn node_edge_provenance(
    file: &str,
    node: tree_sitter::Node,
    symbol_id: &str,
) -> Vec<EdgeProvenance> {
    edge_provenance(file, make_span(node), symbol_id)
}

pub(super) fn span_from_text_range(
    node: tree_sitter::Node,
    text: &str,
    start: usize,
    end: usize,
) -> Option<Span> {
    let bytes = text.as_bytes();
    if start > end || end > bytes.len() {
        return None;
    }

    fn absolute_position(base: tree_sitter::Point, bytes: &[u8]) -> [usize; 2] {
        let newline_count = bytes.iter().filter(|&&byte| byte == b'\n').count();
        if newline_count == 0 {
            [base.row, base.column + bytes.len()]
        } else {
            let last_newline = bytes
                .iter()
                .rposition(|&byte| byte == b'\n')
                .expect("counted newlines above");
            [base.row + newline_count, bytes.len() - last_newline - 1]
        }
    }

    let base = node.start_position();
    Some(Span {
        start: absolute_position(base, &bytes[..start]),
        end: absolute_position(base, &bytes[..end]),
    })
}

pub(super) fn find_user_type_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "user_type" {
            return type_identifier_text(child, source);
        }
    }
    None
}

pub(super) fn find_pattern_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "pattern" {
            return simple_identifier_text(child, source);
        }
    }
    None
}

pub(super) fn extract_swift_doc_comment(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut comments = Vec::new();
    let mut prev = node.prev_named_sibling();
    while let Some(sib) = prev {
        if matches!(sib.kind(), "attribute" | "modifiers") {
            prev = sib.prev_named_sibling();
            continue;
        }
        if sib.kind() == "comment" || sib.kind() == "multiline_comment" {
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

pub(super) fn extract_swift_doc_comment_or_trailing_line_comment(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    extract_swift_doc_comment(node, source)
        .or_else(|| extract_trailing_swift_line_comment(node, source))
}

fn extract_trailing_swift_line_comment(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let start = node.end_byte();
    if start >= source.len() {
        return None;
    }
    let end = source[start..]
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .map(|offset| start + offset)
        .unwrap_or(source.len());
    let suffix = std::str::from_utf8(&source[start..end]).ok()?.trim();
    suffix.starts_with("//").then(|| suffix.to_string())
}

pub(super) fn parse_swift_attribute_name(text: &str) -> Option<String> {
    let trimmed = text.trim().trim_start_matches('@');
    let ident: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.'))
        .collect();
    let base = ident.rsplit('.').next().unwrap_or(&ident).trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

pub(super) fn collect_swift_attribute_names(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.named_children(&mut mod_cursor) {
                if modifier.kind() == "attribute"
                    && let Ok(text) = modifier.utf8_text(source)
                    && let Some(name) = parse_swift_attribute_name(text)
                {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// Check if a Swift node has a specific attribute (e.g., @main, @Observable).
pub(super) fn has_swift_attribute(node: tree_sitter::Node, source: &[u8], attr_name: &str) -> bool {
    collect_swift_attribute_names(node, source)
        .into_iter()
        .any(|name| name == attr_name)
}

pub(super) fn collect_inheritance_names(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "inheritance_specifier"
            && let Some(name) =
                find_user_type_name(child, source).or_else(|| type_identifier_text(child, source))
        {
            names.push(name);
        }
    }
    names
}

/// Walk up from a call node to find an enclosing Swift conditional.
/// Stops at `function_declaration` or `closure_expression` boundary.
pub(super) fn find_enclosing_swift_condition(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration" | "closure_expression" => return None,
            "if_statement" => {
                // Get the condition from the if_statement
                if let Some(cond) = parent.child_by_field_name("condition") {
                    return cond.utf8_text(source).ok().map(|s| s.trim().to_string());
                }
                // Fallback: get text between "if" and "{"
                if let Ok(text) = parent.utf8_text(source)
                    && let Some(if_pos) = text.find("if")
                    && let Some(brace_pos) = text.find('{')
                {
                    let cond = text[if_pos + 2..brace_pos].trim();
                    if !cond.is_empty() {
                        return Some(cond.to_string());
                    }
                }
                return None;
            }
            "guard_statement" => {
                if let Ok(text) = parent.utf8_text(source)
                    && let Some(guard_pos) = text.find("guard")
                    && let Some(else_pos) = text.find("else")
                {
                    let cond = text[guard_pos + 5..else_pos].trim();
                    if !cond.is_empty() {
                        return Some(format!("guard {}", cond));
                    }
                }
                return None;
            }
            "switch_entry" => {
                // Get the case pattern text
                if let Ok(text) = parent.utf8_text(source)
                    && let Some(case_pos) = text.find("case")
                    && let Some(colon_pos) = text[case_pos..].find(':').map(|p| case_pos + p)
                    && colon_pos > case_pos + 4
                {
                    let pattern = text[case_pos + 4..colon_pos].trim();
                    if !pattern.is_empty() {
                        return Some(format!("case {}", pattern));
                    }
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

/// Check if a Swift call node is at an async boundary.
pub(super) fn sanitize_id_component(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

pub(super) fn enclosing_owner_type_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = Some(node);
    while let Some(cursor) = current {
        if cursor.kind() == "class_declaration" {
            return match detect_class_declaration_type(cursor) {
                ClassDeclarationType::Extension => find_user_type_name(cursor, source),
                _ => type_identifier_text(cursor, source),
            };
        }
        current = cursor.parent();
    }
    None
}

pub(super) fn is_builtin_swiftui_view(name: &str) -> bool {
    matches!(
        name,
        "AnyView"
            | "Button"
            | "Color"
            | "Divider"
            | "DisclosureGroup"
            | "EmptyView"
            | "ForEach"
            | "Form"
            | "GeometryReader"
            | "Grid"
            | "GridRow"
            | "Group"
            | "HStack"
            | "Image"
            | "Label"
            | "LazyHGrid"
            | "LazyHStack"
            | "LazyVGrid"
            | "LazyVStack"
            | "Link"
            | "List"
            | "Menu"
            | "NavigationLink"
            | "NavigationStack"
            | "NavigationView"
            | "Picker"
            | "ProgressView"
            | "ScrollView"
            | "Section"
            | "SecureField"
            | "Spacer"
            | "TabView"
            | "Text"
            | "TextField"
            | "TimelineView"
            | "Toggle"
            | "VStack"
            | "ZStack"
    )
}

pub(super) fn type_text_looks_like_swiftui_view(type_text: &str) -> bool {
    let trimmed = normalized_swiftui_type_text(type_text);
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("some View")
        || trimmed.starts_with("any View")
        || trimmed.starts_with("some SwiftUI.View")
        || trimmed.starts_with("any SwiftUI.View")
    {
        return true;
    }

    let ident: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '<' | '>'))
        .collect();
    let base = ident
        .split('<')
        .next()
        .unwrap_or(&ident)
        .rsplit('.')
        .next()
        .unwrap_or(&ident);
    base == "View" || base.ends_with("View") || is_builtin_swiftui_view(base)
}

pub(super) fn normalized_swiftui_type_text(type_text: &str) -> &str {
    let mut trimmed = type_text.trim().trim_start_matches(':').trim();
    loop {
        let without_optional = trimmed.trim_end_matches(['?', '!']).trim();
        let stripped_parens = strip_outer_parentheses(without_optional);
        if stripped_parens == trimmed {
            return stripped_parens;
        }
        trimmed = stripped_parens;
    }
}

pub(super) fn strip_outer_parentheses(text: &str) -> &str {
    if !(text.starts_with('(') && text.ends_with(')')) {
        return text;
    }

    let mut depth = 0usize;
    for (idx, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && idx + ch.len_utf8() != text.len() {
                    return text;
                }
            }
            _ => {}
        }
    }

    if depth == 0 {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

pub(super) fn declaration_returns_swiftui_view(node: tree_sitter::Node, source: &[u8]) -> bool {
    match node.kind() {
        "property_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|child| child.kind() == "type_annotation")
                .and_then(|type_node| type_node.utf8_text(source).ok())
                .is_some_and(type_text_looks_like_swiftui_view)
        }
        "function_declaration" | "protocol_function_declaration" => {
            let Ok(text) = node.utf8_text(source) else {
                return false;
            };
            text.split_once("->")
                .map(|(_, type_text)| type_text_looks_like_swiftui_view(type_text))
                .unwrap_or(false)
        }
        _ => false,
    }
}

pub(super) fn file_matches(node_file: &Path, file_path: &Path) -> bool {
    let node_file = node_file.to_string_lossy();
    let file_path = file_path.to_string_lossy();
    node_file == file_path
        || node_file.ends_with(file_path.as_ref())
        || file_path.ends_with(node_file.as_ref())
        || node_file
            .rsplit('/')
            .next()
            .zip(file_path.rsplit('/').next())
            .is_some_and(|(left, right)| left == right)
}

pub(super) fn swiftui_owner_id(node_id: &str) -> Option<&str> {
    node_id.split_once("::view:").map(|(owner_id, _)| owner_id)
}

pub(super) fn line_matches(node_line: usize, ast_row_zero_based: usize) -> bool {
    node_line.abs_diff(ast_row_zero_based) <= 1
}

pub(super) struct SourceIndex {
    line_starts: Vec<usize>,
}

impl SourceIndex {
    pub(super) fn new(source: &[u8]) -> Self {
        let mut line_starts = vec![0usize];
        for (idx, &byte) in source.iter().enumerate() {
            if byte == b'\n' {
                line_starts.push(idx + 1);
            }
        }
        Self { line_starts }
    }

    pub(super) fn line_at_byte(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(line) => line.saturating_sub(1),
        }
    }

    pub(super) fn byte_offset(&self, line: usize, column: usize, source: &[u8]) -> Option<usize> {
        let start = *self.line_starts.get(line)?;
        let next_line_start = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(source.len());
        let line_slice = source.get(start..next_line_start)?;
        let mut remaining = column;
        for (offset, _) in std::str::from_utf8(line_slice).ok()?.char_indices() {
            if remaining == 0 {
                return Some(start + offset);
            }
            remaining -= 1;
        }
        (remaining == 0).then_some(next_line_start)
    }

    pub(super) fn text_for_span<'a>(&self, source: &'a [u8], span: &Span) -> Option<&'a str> {
        let start = self.byte_offset(span.start[0], span.start[1], source)?;
        let end = self.byte_offset(span.end[0], span.end[1], source)?;
        std::str::from_utf8(source.get(start..end)?).ok()
    }
}

pub(super) fn declaration_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "property_declaration" => find_pattern_name(node, source),
        "function_declaration" => simple_identifier_text(node, source),
        "protocol_function_declaration" => simple_identifier_text(node, source),
        _ => None,
    }
}

pub(super) fn declaration_kind(node: tree_sitter::Node) -> Option<NodeKind> {
    match node.kind() {
        "property_declaration" => Some(NodeKind::Property),
        "function_declaration" | "protocol_function_declaration" => Some(NodeKind::Function),
        _ => None,
    }
}

pub(super) fn normalized_declaration_match_name(name: &str) -> &str {
    let stripped = name
        .strip_prefix("getter:")
        .or_else(|| name.strip_prefix("setter:"))
        .unwrap_or(name);
    stripped
        .split_once('(')
        .map(|(head, _)| head)
        .unwrap_or(stripped)
}

pub(super) fn matching_swiftui_declaration_id(
    context: &EnrichmentContext<'_>,
    file_path: &Path,
    decl_node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    context.matching_declaration_id(file_path, decl_node, source)
}

pub(super) fn matching_body_id(
    context: &EnrichmentContext<'_>,
    file_path: &Path,
    body_node: tree_sitter::Node,
) -> Option<String> {
    context.matching_body_id(file_path, body_node)
}

fn candidate_owner_names(result: &ExtractionResult) -> HashMap<String, Vec<String>> {
    let id_to_name: HashMap<&str, &str> = result
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();
    let mut candidate_owner_names: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &result.edges {
        if edge.kind == EdgeKind::Contains
            && let Some(owner_name) = id_to_name.get(edge.source.as_str())
        {
            candidate_owner_names
                .entry(edge.target.clone())
                .or_default()
                .push((*owner_name).to_string());
        } else if edge.kind == EdgeKind::Implements
            && let Some(owner_name) = id_to_name.get(edge.target.as_str())
        {
            candidate_owner_names
                .entry(edge.source.clone())
                .or_default()
                .push((*owner_name).to_string());
        }
    }
    candidate_owner_names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_index_maps_lines_and_spans() {
        let source = b"alpha\nbeta\nz";
        let index = SourceIndex::new(source);

        assert_eq!(index.line_at_byte(0), 0);
        assert_eq!(index.line_at_byte(6), 1);
        assert_eq!(index.line_at_byte(source.len() - 1), 2);
        assert_eq!(index.byte_offset(1, 2, source), Some(8));

        let span = Span {
            start: [1, 1],
            end: [1, 4],
        };
        assert_eq!(index.text_for_span(source, &span), Some("eta"));

        let eof_span = Span {
            start: [2, 0],
            end: [2, 1],
        };
        assert_eq!(index.byte_offset(2, 1, source), Some(source.len()));
        assert_eq!(index.text_for_span(source, &eof_span), Some("z"));
    }
}
