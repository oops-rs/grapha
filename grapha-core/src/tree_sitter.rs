use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use tree_sitter::{Node as TsNode, Parser};

use crate::extract::{ExtractionResult, LanguageExtractor};
use crate::graph::{Edge, EdgeKind, EdgeProvenance, Node, NodeKind, Span, Visibility};
use crate::resolve::{Import, ImportKind};

#[derive(Debug, Clone, Copy)]
pub struct TreeSitterLanguageConfig {
    pub id: &'static str,
    pub language: fn() -> tree_sitter::Language,
    pub function_types: &'static [&'static str],
    pub class_types: &'static [&'static str],
    pub method_types: &'static [&'static str],
    pub interface_types: &'static [&'static str],
    pub interface_kind: NodeKind,
    pub struct_types: &'static [&'static str],
    pub enum_types: &'static [&'static str],
    pub enum_member_types: &'static [&'static str],
    pub type_alias_types: &'static [&'static str],
    pub import_types: &'static [&'static str],
    pub call_types: &'static [&'static str],
    pub variable_types: &'static [&'static str],
    pub field_types: &'static [&'static str],
    pub property_types: &'static [&'static str],
    pub extra_class_types: &'static [&'static str],
    pub name_field: &'static str,
    pub body_field: &'static str,
    pub methods_are_top_level: bool,
}

pub struct GenericTreeSitterExtractor {
    pub config: &'static TreeSitterLanguageConfig,
}

impl LanguageExtractor for GenericTreeSitterExtractor {
    fn extract(&self, source: &[u8], file_path: &Path) -> anyhow::Result<ExtractionResult> {
        let mut parser = Parser::new();
        parser
            .set_language(&(self.config.language)())
            .map_err(|err| anyhow::anyhow!("failed to load {} grammar: {err}", self.config.id))?;
        let tree = parser.parse(source, None).ok_or_else(|| {
            anyhow::anyhow!("tree-sitter failed to parse {} source", self.config.id)
        })?;

        let root = tree.root_node();
        let mut state = ExtractionState {
            config: self.config,
            source,
            file: file_path.to_string_lossy().to_string(),
            file_node_id: format!("file:{}", file_path.to_string_lossy()),
            result: ExtractionResult::new(),
            scopes: Vec::new(),
        };
        state.push_file_node(root, file_path);
        state.walk(root);
        state.extract_framework_nodes();
        Ok(state.result)
    }
}

#[derive(Clone)]
struct Scope {
    id: String,
    kind: NodeKind,
}

struct ExtractionState<'a> {
    config: &'static TreeSitterLanguageConfig,
    source: &'a [u8],
    file: String,
    file_node_id: String,
    result: ExtractionResult,
    scopes: Vec<Scope>,
}

impl ExtractionState<'_> {
    fn walk(&mut self, node: TsNode) {
        let kind = node.kind();

        if self.config.import_types.contains(&kind)
            && self.extract_import(node)
            && !self.config.call_types.contains(&kind)
        {
            return;
        }

        if self.config.enum_member_types.contains(&kind) && self.extract_enum_member(node) {
            return;
        }

        if self.config.class_types.contains(&kind) || self.config.extra_class_types.contains(&kind)
        {
            self.extract_container(node, NodeKind::Class);
            return;
        }

        if self.config.interface_types.contains(&kind) {
            self.extract_container(node, self.config.interface_kind);
            return;
        }

        if self.config.struct_types.contains(&kind) {
            self.extract_container(node, NodeKind::Struct);
            return;
        }

        if self.config.enum_types.contains(&kind) {
            self.extract_container(node, NodeKind::Enum);
            return;
        }

        if self.config.type_alias_types.contains(&kind)
            && self.extract_leaf(node, NodeKind::TypeAlias)
        {
            return;
        }

        if self.config.field_types.contains(&kind)
            && self.inside_container()
            && self.extract_leaf(node, NodeKind::Field)
        {
            return;
        }

        if self.config.property_types.contains(&kind)
            && self.inside_container()
            && self.extract_leaf(node, NodeKind::Property)
        {
            self.walk_body_or_children(node);
            return;
        }

        if self.config.variable_types.contains(&kind) && self.extract_variable(node) {
            return;
        }

        if (self.config.function_types.contains(&kind) || self.config.method_types.contains(&kind))
            && self.extract_function(node)
        {
            return;
        }

        if self.config.call_types.contains(&kind) {
            self.extract_call(node);
        }

        self.walk_children(node);
    }

    fn walk_children(&mut self, node: TsNode) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child);
        }
    }

    fn walk_body_or_children(&mut self, node: TsNode) {
        if let Some(body) = node.child_by_field_name(self.config.body_field) {
            self.walk_children(body);
        } else {
            self.walk_children(node);
        }
    }

    fn inside_container(&self) -> bool {
        self.scopes.iter().rev().any(|scope| {
            matches!(
                scope.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Module
                    | NodeKind::Extension
            )
        })
    }

    fn current_scope_id(&self) -> Option<&str> {
        self.scopes.last().map(|scope| scope.id.as_str())
    }

    fn current_callable_scope_id(&self) -> Option<&str> {
        self.scopes
            .iter()
            .rev()
            .find(|scope| matches!(scope.kind, NodeKind::Function | NodeKind::Property))
            .map(|scope| scope.id.as_str())
    }

    fn extract_container(&mut self, node: TsNode, mut node_kind: NodeKind) -> bool {
        node_kind = self.classify_container_kind(node, node_kind);
        let Some(name) = self.name_for_node(node) else {
            self.walk_children(node);
            return true;
        };

        let Some(id) = self.push_node(node, node_kind, name, true) else {
            return true;
        };
        self.extract_inheritance(node, &id, node_kind);
        self.scopes.push(Scope {
            id,
            kind: node_kind,
        });
        self.walk_body_or_children(node);
        self.scopes.pop();
        true
    }

    fn extract_function(&mut self, node: TsNode) -> bool {
        let inside_container = self.inside_container();
        if self.config.method_types.contains(&node.kind())
            && !inside_container
            && !self.config.methods_are_top_level
            && !self.config.function_types.contains(&node.kind())
        {
            return false;
        }

        let Some(name) = self.name_for_node(node) else {
            self.walk_body_or_children(node);
            return true;
        };
        if is_anonymous_name(&name) {
            self.walk_body_or_children(node);
            return true;
        }

        let Some(id) = self.push_node(node, NodeKind::Function, name, false) else {
            return true;
        };
        self.scopes.push(Scope {
            id,
            kind: NodeKind::Function,
        });
        self.walk_body_or_children(node);
        self.scopes.pop();
        true
    }

    fn extract_leaf(&mut self, node: TsNode, kind: NodeKind) -> bool {
        let Some(name) = self.name_for_node(node) else {
            return false;
        };
        self.push_node(node, kind, name, false).is_some()
    }

    fn extract_enum_member(&mut self, node: TsNode) -> bool {
        let Some(name) = self.name_for_node(node) else {
            return false;
        };
        self.push_node(node, NodeKind::Variant, name, false)
            .is_some()
    }

    fn extract_variable(&mut self, node: TsNode) -> bool {
        let mut handled = false;

        let declarators = descendants_with_kinds(
            node,
            &[
                "variable_declarator",
                "init_declarator",
                "const_declaration",
                "short_var_declaration",
                "assignment",
            ],
        );
        if !declarators.is_empty() {
            for declarator in declarators {
                handled |= self.extract_variable_declarator(declarator, node);
            }
            return handled;
        }

        self.extract_variable_declarator(node, node)
    }

    fn extract_variable_declarator(&mut self, node: TsNode, declaration: TsNode) -> bool {
        let Some(name_node) = node
            .child_by_field_name("name")
            .or_else(|| first_identifier(node))
        else {
            return false;
        };
        let Some(name) = text(name_node, self.source).map(clean_identifier) else {
            return false;
        };

        if let Some(value) = node.child_by_field_name("value")
            && matches!(value.kind(), "arrow_function" | "function_expression")
        {
            let Some(id) =
                self.push_named_node(value, NodeKind::Function, name, declaration, false)
            else {
                return true;
            };
            self.scopes.push(Scope {
                id,
                kind: NodeKind::Function,
            });
            self.walk_body_or_children(value);
            self.scopes.pop();
            return true;
        }

        if self.inside_container() {
            return false;
        }

        let node_kind = if declaration_text_contains(declaration, self.source, "const") {
            NodeKind::Constant
        } else {
            NodeKind::Variable
        };
        self.push_named_node(node, node_kind, name, declaration, false)
            .is_some()
    }

    fn extract_call(&mut self, node: TsNode) {
        let Some(source_id) = self.current_callable_scope_id().map(ToString::to_string) else {
            return;
        };
        let Some(callee) = callee_name(node, self.source) else {
            return;
        };
        if should_skip_call(&callee) {
            return;
        }

        self.result.edges.push(Edge {
            source: source_id.clone(),
            target: callee,
            kind: EdgeKind::Calls,
            confidence: 0.6,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: edge_provenance(&self.file, node, &source_id),
            repo: None,
        });
    }

    fn extract_import(&mut self, node: TsNode) -> bool {
        let Some(raw) = text(node, self.source) else {
            return false;
        };
        let imports = parse_imports(self.config.id, &raw);
        if imports.is_empty() {
            return false;
        }
        for import in imports {
            self.push_import_node(node, &import.path, &raw);
            self.result.imports.push(import);
        }
        true
    }

    fn extract_inheritance(&mut self, node: TsNode, source_id: &str, owner_kind: NodeKind) {
        for name in inheritance_names(node, self.source) {
            let edge_kind = if matches!(owner_kind, NodeKind::Trait | NodeKind::Protocol) {
                EdgeKind::Inherits
            } else if looks_like_interface_name(&name) {
                EdgeKind::Implements
            } else {
                EdgeKind::Inherits
            };
            self.result.edges.push(Edge {
                source: source_id.to_string(),
                target: name,
                kind: edge_kind,
                confidence: 0.55,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: edge_provenance(&self.file, node, source_id),
                repo: None,
            });
        }
    }

    fn push_node(
        &mut self,
        node: TsNode,
        kind: NodeKind,
        name: String,
        is_container: bool,
    ) -> Option<String> {
        self.push_named_node(node, kind, name, node, is_container)
    }

    fn push_named_node(
        &mut self,
        node: TsNode,
        kind: NodeKind,
        name: String,
        position_node: TsNode,
        is_container: bool,
    ) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let parent_id = self.current_scope_id().map(ToString::to_string);
        let proposed_id = make_decl_id(&self.file, parent_id.as_deref(), &name);
        let id = unique_id(&self.result, proposed_id, position_node);
        let contains_source = parent_id.unwrap_or_else(|| self.file_node_id.clone());
        if self
            .result
            .nodes
            .iter()
            .any(|node| node.id == contains_source)
        {
            self.result.edges.push(Edge {
                source: contains_source.clone(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: edge_provenance(&self.file, position_node, &contains_source),
                repo: None,
            });
        }

        let mut metadata = HashMap::new();
        metadata.insert("language".to_string(), self.config.id.to_string());
        if declaration_text_contains(node, self.source, "async")
            || declaration_text_contains(node, self.source, "suspend")
        {
            metadata.insert("async".to_string(), "true".to_string());
        }
        if declaration_text_contains(node, self.source, "static") {
            metadata.insert("static".to_string(), "true".to_string());
        }
        if is_exported(node, self.source) {
            metadata.insert("exported".to_string(), "true".to_string());
        }

        self.result.nodes.push(Node {
            id: id.clone(),
            kind,
            name,
            file: self.file.clone().into(),
            span: make_span(position_node),
            visibility: visibility(node, self.source),
            metadata,
            role: None,
            signature: signature_for(node, self.source, self.config.body_field, is_container),
            doc_comment: doc_comment_before(node, self.source),
            module: None,
            snippet: None,
            repo: None,
        });

        Some(id)
    }

    fn push_file_node(&mut self, root: TsNode, file_path: &Path) {
        let mut metadata = HashMap::new();
        metadata.insert("language".to_string(), self.config.id.to_string());

        let name = file_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&self.file)
            .to_string();

        self.result.nodes.push(Node {
            id: self.file_node_id.clone(),
            kind: NodeKind::File,
            name,
            file: self.file.clone().into(),
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

    fn push_import_node(&mut self, node: TsNode, name: &str, signature: &str) -> Option<String> {
        let name = clean_identifier(name.to_string());
        if name.is_empty() {
            return None;
        }

        let proposed_id = make_decl_id(&self.file, None, &format!("import:{name}"));
        let id = unique_id(&self.result, proposed_id, node);
        self.result.edges.push(Edge {
            source: self.file_node_id.clone(),
            target: id.clone(),
            kind: EdgeKind::Contains,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: edge_provenance(&self.file, node, &self.file_node_id),
            repo: None,
        });

        let mut metadata = HashMap::new();
        metadata.insert("language".to_string(), self.config.id.to_string());

        self.result.nodes.push(Node {
            id: id.clone(),
            kind: NodeKind::Import,
            name,
            file: self.file.clone().into(),
            span: make_span(node),
            visibility: Visibility::Private,
            metadata,
            role: None,
            signature: Some(signature.trim().to_string()),
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        });
        self.result.edges.push(Edge {
            source: self.file_node_id.clone(),
            target: id.clone(),
            kind: EdgeKind::Imports,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: edge_provenance(&self.file, node, &self.file_node_id),
            repo: None,
        });

        Some(id)
    }

    fn name_for_node(&self, node: TsNode) -> Option<String> {
        node.child_by_field_name(self.config.name_field)
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.child_by_field_name("declarator"))
            .and_then(|child| terminal_name(child, self.source))
            .or_else(|| first_identifier(node).and_then(|child| terminal_name(child, self.source)))
            .map(clean_identifier)
            .filter(|name| !name.is_empty())
    }

    fn classify_container_kind(&self, node: TsNode, fallback: NodeKind) -> NodeKind {
        let Ok(raw) = node.utf8_text(self.source) else {
            return fallback;
        };
        let trimmed = raw.trim_start();
        if self.config.id == "swift" {
            if trimmed.starts_with("struct ") {
                return NodeKind::Struct;
            }
            if trimmed.starts_with("enum ") {
                return NodeKind::Enum;
            }
        }
        if self.config.id == "kotlin" {
            if trimmed.starts_with("interface ") || trimmed.starts_with("fun interface ") {
                return NodeKind::Trait;
            }
            if trimmed.starts_with("enum class ") {
                return NodeKind::Enum;
            }
        }
        if self.config.id == "php" && trimmed.starts_with("trait ") {
            return NodeKind::Trait;
        }
        fallback
    }

    fn extract_framework_nodes(&mut self) {
        let Ok(content) = std::str::from_utf8(self.source) else {
            return;
        };

        match self.config.id {
            "typescript" | "tsx" | "javascript" => {
                self.extract_javascript_framework_nodes(content);
            }
            "python" => self.extract_python_framework_nodes(content),
            "go" => self.extract_go_framework_nodes(content),
            "java" => self.extract_java_framework_nodes(content),
            "csharp" => self.extract_csharp_framework_nodes(content),
            "php" => self.extract_php_framework_nodes(content),
            "ruby" => self.extract_ruby_framework_nodes(content),
            _ => {}
        }
    }

    fn extract_javascript_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(r#"(?:app|router)\s*\.\s*(get|post|put|patch|delete|all|use)\s*\(\s*["']([^"']+)["']"#),
            1,
            2,
        );

        if self.file.ends_with(".tsx") || self.file.ends_with(".jsx") {
            for pattern in [
                r#"(?:export\s+)?function\s+([A-Z][A-Za-z0-9]*)\s*\("#,
                r#"(?:export\s+)?(?:const|let)\s+([A-Z][A-Za-z0-9]*)\s*=\s*(?:\([^)]*\)|[A-Za-z_][A-Za-z0-9_]*)\s*=>"#,
                r#"(?:export\s+)?(?:const|let)\s+([A-Z][A-Za-z0-9]*)\s*=\s*(?:React\.)?(?:forwardRef|memo)"#,
            ] {
                let re = regex(pattern);
                for capture in re.captures_iter(content) {
                    let Some(full_match) = capture.get(0) else {
                        continue;
                    };
                    let Some(name) = capture.get(1).map(|m| m.as_str()) else {
                        continue;
                    };
                    let lookahead = content
                        .get(full_match.end()..)
                        .unwrap_or("")
                        .chars()
                        .take(500)
                        .collect::<String>();
                    if lookahead.contains('<')
                        && (lookahead.contains("/>") || lookahead.contains("</"))
                    {
                        self.push_synthetic_node(
                            NodeKind::Component,
                            "component",
                            name,
                            full_match.start(),
                            full_match.end(),
                            component_metadata(self.config.id),
                        );
                    }
                }
            }
        }

        if (self.file.contains("/pages/")
            || self.file.starts_with("pages/")
            || self.file.contains("/app/")
            || self.file.starts_with("app/"))
            && let Some(export_index) = content.find("export default")
            && let Some(route) = nextjs_route_path(&self.file)
        {
            self.push_synthetic_node(
                NodeKind::Route,
                "route",
                &route,
                export_index,
                export_index + "export default".len(),
                route_metadata(self.config.id, None, Some(&route)),
            );
        }
    }

    fn extract_python_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(r#"@\w+\.route\s*\(\s*['"]([^'"]+)['"]"#),
            0,
            1,
        );
        self.push_route_matches(
            content,
            &regex(r#"@\w+\.(get|post|put|patch|delete|options|head)\s*\(\s*['"]([^'"]+)['"]"#),
            1,
            2,
        );
        self.push_route_matches(content, &regex(r#"path\s*\(\s*['"]([^'"]+)['"]"#), 0, 1);
    }

    fn extract_go_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(r#"\.\s*(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD)\s*\(\s*["']([^"']+)["']"#),
            1,
            2,
        );
        self.push_route_matches(
            content,
            &regex(r#"\.\s*(Get|Post|Put|Patch|Delete|Options|Head)\s*\(\s*["']([^"']+)["']"#),
            1,
            2,
        );
        self.push_route_matches(
            content,
            &regex(r#"HandleFunc\s*\(\s*["']([^"']+)["']"#),
            0,
            1,
        );
    }

    fn extract_java_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(r#"@(Get|Post|Put|Patch|Delete)Mapping\s*(?:\(\s*(?:value\s*=\s*)?["']([^"']+)["'])?"#),
            1,
            2,
        );
        self.push_route_matches(
            content,
            &regex(r#"@RequestMapping\s*\(\s*(?:value\s*=\s*)?["']([^"']+)["']"#),
            0,
            1,
        );
    }

    fn extract_csharp_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(
                r#"\[Http(Get|Post|Put|Patch|Delete|Head|Options)(?:\s*\(\s*["']([^"']+)["'])?"#,
            ),
            1,
            2,
        );
        self.push_route_matches(
            content,
            &regex(r#"\.Map(Get|Post|Put|Patch|Delete)\s*\(\s*["']([^"']+)["']"#),
            1,
            2,
        );
    }

    fn extract_php_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(r#"Route::(get|post|put|patch|delete|any|resource)\s*\(\s*['"]([^'"]+)['"]"#),
            1,
            2,
        );
    }

    fn extract_ruby_framework_nodes(&mut self, content: &str) {
        self.push_route_matches(
            content,
            &regex(r#"(?m)^\s*(get|post|put|patch|delete)\s+['"]([^'"]+)['"]"#),
            1,
            2,
        );
        self.push_route_matches(
            content,
            &regex(r#"(?m)^\s*resources\s+:([A-Za-z_][A-Za-z0-9_]*)"#),
            0,
            1,
        );
        self.push_route_matches(content, &regex(r#"(?m)^\s*root\s+['"]([^'"]+)['"]"#), 0, 1);
    }

    fn push_route_matches(
        &mut self,
        content: &str,
        re: &Regex,
        method_group: usize,
        path_group: usize,
    ) {
        for capture in re.captures_iter(content) {
            let Some(full_match) = capture.get(0) else {
                continue;
            };
            let method = if method_group == 0 {
                None
            } else {
                capture
                    .get(method_group)
                    .map(|m| normalize_http_method(m.as_str()))
            };
            let path = capture
                .get(path_group)
                .map(|m| normalize_route_path(m.as_str()))
                .unwrap_or_default();
            if path.is_empty() && method.is_none() {
                continue;
            }
            let name = route_name(method.as_deref(), &path);
            self.push_synthetic_node(
                NodeKind::Route,
                "route",
                &name,
                full_match.start(),
                full_match.end(),
                route_metadata(self.config.id, method.as_deref(), Some(&path)),
            );
        }
    }

    fn push_synthetic_node(
        &mut self,
        kind: NodeKind,
        id_prefix: &str,
        name: &str,
        start_byte: usize,
        end_byte: usize,
        mut metadata: HashMap<String, String>,
    ) {
        if name.is_empty() {
            return;
        }
        let span = span_from_byte_range(self.source, start_byte, end_byte);
        let line = span.start[0] + 1;
        let proposed_id = format!("{id_prefix}:{}:{name}:{line}", self.file);
        let id = unique_synthetic_id(&self.result, proposed_id, &span);
        if self.result.nodes.iter().any(|node| node.id == id) {
            return;
        }
        metadata.insert("language".to_string(), self.config.id.to_string());

        self.result.nodes.push(Node {
            id: id.clone(),
            kind,
            name: name.to_string(),
            file: self.file.clone().into(),
            span: span.clone(),
            visibility: Visibility::Public,
            metadata,
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        });
        self.result.edges.push(Edge {
            source: self.file_node_id.clone(),
            target: id,
            kind: EdgeKind::Contains,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: vec![EdgeProvenance {
                file: self.file.clone().into(),
                span,
                symbol_id: self.file_node_id.clone(),
            }],
            repo: None,
        });
    }
}

fn make_decl_id(file: &str, parent_id: Option<&str>, name: &str) -> String {
    parent_id
        .map(|parent| format!("{parent}::{name}"))
        .unwrap_or_else(|| format!("{file}::{name}"))
}

fn unique_id(result: &ExtractionResult, proposed: String, node: TsNode) -> String {
    if result.nodes.iter().all(|existing| existing.id != proposed) {
        return proposed;
    }
    let span = make_span(node);
    format!(
        "{proposed}@{}:{}:{}:{}",
        span.start[0], span.start[1], span.end[0], span.end[1]
    )
}

fn unique_synthetic_id(result: &ExtractionResult, proposed: String, span: &Span) -> String {
    if result.nodes.iter().all(|existing| existing.id != proposed) {
        return proposed;
    }
    format!(
        "{proposed}@{}:{}:{}:{}",
        span.start[0], span.start[1], span.end[0], span.end[1]
    )
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("framework extractor regex should compile")
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

fn normalize_http_method(method: &str) -> String {
    method.to_ascii_uppercase()
}

fn normalize_route_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        String::new()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn route_name(method: Option<&str>, path: &str) -> String {
    match (method, path.is_empty()) {
        (Some(method), true) => method.to_string(),
        (Some(method), false) => format!("{method} {path}"),
        (None, _) => path.to_string(),
    }
}

fn route_metadata(
    language: &str,
    method: Option<&str>,
    path: Option<&str>,
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert("framework.kind".to_string(), "route".to_string());
    metadata.insert("language".to_string(), language.to_string());
    if let Some(method) = method {
        metadata.insert("route.method".to_string(), method.to_string());
    }
    if let Some(path) = path {
        metadata.insert("route.path".to_string(), path.to_string());
    }
    metadata
}

fn component_metadata(language: &str) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert("framework.kind".to_string(), "component".to_string());
    metadata.insert("language".to_string(), language.to_string());
    metadata
}

fn nextjs_route_path(file: &str) -> Option<String> {
    let normalized = file.replace('\\', "/");
    if let Some(after_pages) = normalized.strip_prefix("pages/") {
        return Some(route_path_from_file(after_pages));
    }
    if let Some(after_pages) = normalized.split("/pages/").nth(1) {
        return Some(route_path_from_file(after_pages));
    }
    if let Some(after_app) = normalized.strip_prefix("app/")
        && (after_app.ends_with("/page.tsx")
            || after_app.ends_with("/page.ts")
            || after_app.ends_with("/page.jsx")
            || after_app.ends_with("/page.js"))
    {
        let route_file = after_app.trim_end_matches("/page.tsx");
        let route_file = route_file.trim_end_matches("/page.ts");
        let route_file = route_file.trim_end_matches("/page.jsx");
        let route_file = route_file.trim_end_matches("/page.js");
        return Some(route_path_from_file(route_file));
    }
    if let Some(after_app) = normalized.split("/app/").nth(1)
        && (after_app.ends_with("/page.tsx")
            || after_app.ends_with("/page.ts")
            || after_app.ends_with("/page.jsx")
            || after_app.ends_with("/page.js"))
    {
        let route_file = after_app.trim_end_matches("/page.tsx");
        let route_file = route_file.trim_end_matches("/page.ts");
        let route_file = route_file.trim_end_matches("/page.jsx");
        let route_file = route_file.trim_end_matches("/page.js");
        return Some(route_path_from_file(route_file));
    }
    None
}

fn route_path_from_file(path: &str) -> String {
    let mut path = path
        .trim_end_matches(".tsx")
        .trim_end_matches(".ts")
        .trim_end_matches(".jsx")
        .trim_end_matches(".js")
        .trim_end_matches("/index")
        .replace("[", ":")
        .replace("]", "");
    if path == "index" {
        path.clear();
    }
    let route = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    route.replace("//", "/")
}

fn make_span(node: TsNode) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span {
        start: [start.row, start.column],
        end: [end.row, end.column],
    }
}

fn edge_provenance(file: &str, node: TsNode, symbol_id: &str) -> Vec<EdgeProvenance> {
    vec![EdgeProvenance {
        file: file.into(),
        span: make_span(node),
        symbol_id: symbol_id.to_string(),
    }]
}

fn text(node: TsNode, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(ToString::to_string)
}

fn declaration_text_contains(node: TsNode, source: &[u8], needle: &str) -> bool {
    node.utf8_text(source).is_ok_and(|raw| raw.contains(needle))
}

fn signature_for(
    node: TsNode,
    source: &[u8],
    body_field: &str,
    is_container: bool,
) -> Option<String> {
    if is_container {
        return None;
    }
    let raw = node.utf8_text(source).ok()?.trim();
    let signature = if let Some(body) = node.child_by_field_name(body_field) {
        let end = body.start_byte().saturating_sub(node.start_byte());
        raw.get(..end).unwrap_or(raw).trim()
    } else if let Some(brace) = raw.find('{') {
        raw[..brace].trim()
    } else {
        raw.lines().next().unwrap_or(raw).trim()
    };
    if signature.is_empty() {
        None
    } else {
        Some(signature.to_string())
    }
}

fn visibility(node: TsNode, source: &[u8]) -> Visibility {
    let raw = node.utf8_text(source).unwrap_or("");
    if raw.contains("public") || raw.contains("export ") || raw.contains("pub ") {
        Visibility::Public
    } else if raw.contains("protected") || raw.contains("internal") || raw.contains("pub(crate)") {
        Visibility::Crate
    } else {
        Visibility::Private
    }
}

fn is_exported(node: TsNode, source: &[u8]) -> bool {
    node.utf8_text(source)
        .is_ok_and(|raw| raw.trim_start().starts_with("export ") || raw.contains("\nexport "))
        || ancestors(node).any(|ancestor| ancestor.kind() == "export_statement")
}

fn doc_comment_before(node: TsNode, source: &[u8]) -> Option<String> {
    let source = std::str::from_utf8(source).ok()?;
    let before = source.get(..node.start_byte())?;
    let mut comments = Vec::new();
    for line in before.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if comments.is_empty() {
                continue;
            }
            break;
        }
        let comment = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
            .or_else(|| trimmed.strip_prefix("//"))
            .or_else(|| trimmed.strip_prefix('#'))
            .map(str::trim);
        if let Some(comment) = comment {
            comments.push(comment.to_string());
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

fn first_identifier(node: TsNode) -> Option<TsNode> {
    if is_identifier_kind(node.kind()) {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_identifier(child) {
            return Some(found);
        }
    }
    None
}

fn terminal_name(node: TsNode, source: &[u8]) -> Option<String> {
    if is_identifier_kind(node.kind()) {
        return text(node, source);
    }

    if matches!(
        node.kind(),
        "pointer_declarator"
            | "reference_declarator"
            | "function_declarator"
            | "parenthesized_declarator"
            | "init_declarator"
            | "qualified_identifier"
            | "scoped_identifier"
            | "member_expression"
            | "selector_expression"
            | "field_expression"
            | "navigation_expression"
    ) {
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        for child in children.iter().rev() {
            if let Some(name) = terminal_name(*child, source) {
                return Some(name);
            }
        }
    }

    first_identifier(node).and_then(|child| text(child, source))
}

fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "simple_identifier"
            | "field_identifier"
            | "property_identifier"
            | "constant"
            | "constant_identifier"
            | "namespace_name"
            | "name"
            | "variable_name"
    ) || kind.ends_with("_identifier")
}

fn clean_identifier(name: String) -> String {
    name.trim()
        .trim_matches('`')
        .trim_start_matches('$')
        .to_string()
}

fn is_anonymous_name(name: &str) -> bool {
    matches!(name, "<anonymous>" | "_" | "")
}

fn descendants_with_kinds<'tree>(node: TsNode<'tree>, kinds: &[&str]) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            out.push(child);
        } else {
            out.extend(descendants_with_kinds(child, kinds));
        }
    }
    out
}

fn callee_name(node: TsNode, source: &[u8]) -> Option<String> {
    node.child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("method"))
        .or_else(|| node.child_by_field_name("selector"))
        .or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor).next()
        })
        .and_then(|callee| terminal_name(callee, source))
        .map(clean_identifier)
}

fn should_skip_call(name: &str) -> bool {
    matches!(
        name,
        "if" | "for" | "while" | "switch" | "return" | "require" | "require_relative"
    )
}

fn parse_imports(language: &str, raw: &str) -> Vec<Import> {
    let trimmed = raw.trim();
    match language {
        "ruby" if !trimmed.starts_with("require") => return Vec::new(),
        "python" => return parse_python_import(trimmed),
        "c" | "cpp" => return parse_c_include(trimmed),
        "go" | "dart" | "ruby" => {
            return quoted_strings(trimmed)
                .into_iter()
                .map(module_import)
                .collect();
        }
        _ => {}
    }

    if let Some(from_module) = module_after_keyword(trimmed, "from") {
        return vec![module_import(from_module)];
    }
    if let Some(after_import) = module_after_keyword(trimmed, "import") {
        return vec![module_import(after_import)];
    }
    if let Some(after_using) = module_after_keyword(trimmed, "using") {
        return vec![module_import(after_using)];
    }
    if let Some(after_use) = module_after_keyword(trimmed, "use") {
        return vec![module_import(after_use)];
    }

    quoted_strings(trimmed)
        .into_iter()
        .map(module_import)
        .collect()
}

fn parse_python_import(raw: &str) -> Vec<Import> {
    if let Some(module) = module_after_keyword(raw, "from") {
        return vec![module_import(module)];
    }
    raw.strip_prefix("import ")
        .map(|rest| {
            rest.split(',')
                .filter_map(|part| part.split_whitespace().next())
                .map(|module| module_import(module.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_c_include(raw: &str) -> Vec<Import> {
    if let Some(start) = raw.find('<')
        && let Some(end) = raw[start + 1..].find('>')
    {
        return vec![module_import(raw[start + 1..start + 1 + end].to_string())];
    }
    quoted_strings(raw).into_iter().map(module_import).collect()
}

fn module_after_keyword(raw: &str, keyword: &str) -> Option<String> {
    let marker = format!("{keyword} ");
    let start = raw.find(&marker)? + marker.len();
    let rest = raw[start..].trim();
    let module = rest
        .split([';', '\n', '{', '}', ','])
        .next()
        .unwrap_or(rest)
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if module.is_empty() {
        None
    } else {
        Some(module.to_string())
    }
}

fn quoted_strings(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = raw.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch != '"' && ch != '\'' {
            continue;
        }
        for (end, current) in chars.by_ref() {
            if current == ch {
                if end > start + 1 {
                    out.push(raw[start + 1..end].to_string());
                }
                break;
            }
        }
    }
    out
}

fn module_import(path: String) -> Import {
    let kind = if path.starts_with('.') {
        ImportKind::Relative
    } else {
        ImportKind::Module
    };
    Import {
        path,
        symbols: Vec::new(),
        kind,
    }
}

fn inheritance_names(node: TsNode, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let fields = [
        "superclass",
        "superclasses",
        "interfaces",
        "super_interfaces",
        "extends",
        "extends_clause",
        "implements",
        "implements_clause",
        "base_list",
        "delegation_specifier",
    ];
    for field in fields {
        if let Some(child) = node.child_by_field_name(field) {
            collect_type_names(child, source, &mut names);
        }
    }

    let raw = node.utf8_text(source).unwrap_or("");
    for keyword in ["extends", "implements", ":"] {
        if let Some(index) = raw.find(keyword) {
            let tail = &raw[index + keyword.len()..];
            let end = tail.find(['{', '(', '\n']).unwrap_or(tail.len());
            for part in tail[..end].split(',') {
                let name = part
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .trim_matches(['<', '>', ':'])
                    .to_string();
                if !name.is_empty() && !names.iter().any(|existing| existing == &name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn collect_type_names(node: TsNode, source: &[u8], names: &mut Vec<String>) {
    if let Some(name) = terminal_name(node, source).map(clean_identifier)
        && !name.is_empty()
        && !names.iter().any(|existing| existing == &name)
    {
        names.push(name);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_names(child, source, names);
    }
}

fn looks_like_interface_name(name: &str) -> bool {
    name.starts_with('I') || name.ends_with("able") || name.ends_with("Protocol")
}

fn ancestors(mut node: TsNode) -> impl Iterator<Item = TsNode> {
    std::iter::from_fn(move || {
        node = node.parent()?;
        Some(node)
    })
}
