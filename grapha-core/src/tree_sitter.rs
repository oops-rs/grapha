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
            self.walk_initializers(node, false);
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
            self.walk_initializers(node, true);
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

    fn walk_initializers(&mut self, declaration: TsNode, skip_function_values: bool) {
        let mut initializers = Vec::new();
        if let Some(value) = initializer_value(declaration) {
            initializers.push(value);
        } else {
            let mut cursor = declaration.walk();
            for child in declaration.named_children(&mut cursor) {
                if is_variable_declarator_kind(child.kind())
                    && let Some(value) = initializer_value(child)
                {
                    initializers.push(value);
                }
            }
        }

        for initializer in initializers {
            if is_function_value(initializer) {
                if !skip_function_values {
                    self.walk_function_value_body(initializer);
                }
            } else {
                self.walk(initializer);
            }
        }
    }

    fn walk_function_value_body(&mut self, value: TsNode) {
        if let Some(body) = value.child_by_field_name(self.config.body_field) {
            self.walk(body);
        } else {
            self.walk_children(value);
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

    fn inside_callable_scope(&self) -> bool {
        self.current_callable_scope_id().is_some()
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
            self.walk_function_value_body(value);
            self.scopes.pop();
            return true;
        }

        if self.inside_callable_scope() {
            // Report ordinary locals through their enclosing callable instead
            // of materializing short-lived symbols. Returning handled keeps
            // `walk_initializers` responsible for their call edges.
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
        let source_id = self
            .current_callable_scope_id()
            .map(ToString::to_string)
            .unwrap_or_else(|| self.file_node_id.clone());
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
        let imports = parse_imports(self.config.id, node, self.source, &raw);
        if imports.is_empty() {
            return false;
        }
        for import in imports {
            // Kotlin's grammar exposes every declaration as an individual
            // `import` node. Keep those imports as resolution facts, but do
            // not materialize tens of thousands of graph symbols in Android
            // projects for declarations that do not represent code symbols.
            // Other grammars retain their established import-node behavior.
            if self.config.id != "kotlin" {
                self.push_import_node(node, &import.path, &raw);
            }
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
        if self.config.id == "kotlin"
            && kind == NodeKind::Field
            && node.kind() == "property_declaration"
            && let Some(declared_type) =
                kotlin_field_declared_type(node, self.source, &self.result.imports)
        {
            metadata.insert("grapha.declared_type".to_string(), declared_type);
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
        if self.config.id == "kotlin" && node.kind() == "secondary_constructor" {
            return Some("constructor".to_string());
        }
        if self.config.id == "kotlin" && node.kind() == "companion_object" {
            return node
                .child_by_field_name(self.config.name_field)
                .or_else(|| node.child_by_field_name("name"))
                .and_then(|child| terminal_name(child, self.source))
                .map(clean_identifier)
                .filter(|name| !name.is_empty())
                .or_else(|| Some("Companion".to_string()));
        }
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

fn initializer_value(node: TsNode) -> Option<TsNode> {
    node.child_by_field_name("value")
        .or_else(|| node.child_by_field_name("right"))
}

fn is_function_value(node: TsNode) -> bool {
    matches!(node.kind(), "arrow_function" | "function_expression")
}

fn is_variable_declarator_kind(kind: &str) -> bool {
    matches!(
        kind,
        "variable_declarator"
            | "init_declarator"
            | "const_declaration"
            | "short_var_declaration"
            | "assignment"
    )
}

/// Prove a Kotlin field's declared type without inferring from arbitrary
/// delegates. An explicit annotation always wins: an unsupported explicit
/// annotation deliberately blocks the narrower AndroidX-delegate fallback.
fn kotlin_field_declared_type(
    property: TsNode,
    source: &[u8],
    imports: &[Import],
) -> Option<String> {
    match kotlin_explicit_property_type(property, source) {
        KotlinExplicitPropertyType::Supported(name) => return Some(name),
        KotlinExplicitPropertyType::Unsupported => return None,
        KotlinExplicitPropertyType::Absent => {}
    }

    kotlin_androidx_view_model_delegate_type(property, source, imports)
}

enum KotlinExplicitPropertyType {
    Absent,
    Supported(String),
    Unsupported,
}

/// A Kotlin property's variable declaration contains its annotation directly.
/// Only a single unqualified `user_type` identifier is a stable nominal type;
/// the nullable spelling wraps that same shape in `nullable_type`.
fn kotlin_explicit_property_type(property: TsNode, source: &[u8]) -> KotlinExplicitPropertyType {
    let Some(variable_declaration) = direct_named_children(property)
        .into_iter()
        .find(|child| child.kind() == "variable_declaration")
    else {
        return KotlinExplicitPropertyType::Absent;
    };

    let type_children = direct_named_children(variable_declaration)
        .into_iter()
        .filter(|child| !matches!(child.kind(), "identifier" | "annotation"))
        .collect::<Vec<_>>();
    let [type_node] = type_children.as_slice() else {
        return if type_children.is_empty() {
            KotlinExplicitPropertyType::Absent
        } else {
            KotlinExplicitPropertyType::Unsupported
        };
    };

    if let Some(name) = kotlin_simple_nominal_type(*type_node, source) {
        KotlinExplicitPropertyType::Supported(name)
    } else {
        KotlinExplicitPropertyType::Unsupported
    }
}

fn kotlin_androidx_view_model_delegate_type(
    property: TsNode,
    source: &[u8],
    imports: &[Import],
) -> Option<String> {
    let delegates = direct_named_children(property)
        .into_iter()
        .filter(|child| child.kind() == "property_delegate")
        .collect::<Vec<_>>();
    let [delegate] = delegates.as_slice() else {
        return None;
    };

    let delegate_children = direct_named_children(*delegate);
    let [call] = delegate_children.as_slice() else {
        return None;
    };
    if call.kind() != "call_expression" {
        return None;
    }

    let call_children = direct_named_children(*call);
    let callee = call_children.first().copied()?;
    if callee.kind() != "identifier" {
        return None;
    }
    let callee = clean_identifier(text(callee, source)?);
    let required_import = match callee.as_str() {
        "viewModels" => "androidx.fragment.app.viewModels",
        "activityViewModels" => "androidx.fragment.app.activityViewModels",
        _ => return None,
    };
    if !imports.iter().any(|import| {
        import.kind == ImportKind::Module
            && import.symbols.is_empty()
            && import.path == required_import
    }) {
        return None;
    }

    let type_arguments = call_children
        .into_iter()
        .filter(|child| child.kind() == "type_arguments")
        .collect::<Vec<_>>();
    let [type_arguments] = type_arguments.as_slice() else {
        return None;
    };
    let projections = direct_named_children(*type_arguments);
    let [projection] = projections.as_slice() else {
        return None;
    };
    if projection.kind() != "type_projection" {
        return None;
    }
    let projected_types = direct_named_children(*projection);
    let [projected_type] = projected_types.as_slice() else {
        return None;
    };
    if projected_type.kind() != "user_type" {
        return None;
    }

    kotlin_simple_user_type(*projected_type, source)
}

fn kotlin_simple_nominal_type(node: TsNode, source: &[u8]) -> Option<String> {
    match node.kind() {
        "user_type" => kotlin_simple_user_type(node, source),
        "nullable_type" => {
            let children = direct_named_children(node);
            let [user_type] = children.as_slice() else {
                return None;
            };
            kotlin_simple_user_type(*user_type, source)
        }
        _ => None,
    }
}

fn kotlin_simple_user_type(node: TsNode, source: &[u8]) -> Option<String> {
    if node.kind() != "user_type" {
        return None;
    }
    let children = direct_named_children(node);
    let [identifier] = children.as_slice() else {
        return None;
    };
    if identifier.kind() != "identifier" {
        return None;
    }
    text(*identifier, source)
        .map(clean_identifier)
        .filter(|name| !name.is_empty())
}

fn direct_named_children(node: TsNode) -> Vec<TsNode> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
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
    if let Some(qualified) = qualified_method_invocation_name(node, source) {
        return Some(qualified);
    }

    node.child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("method"))
        .or_else(|| node.child_by_field_name("selector"))
        .or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor).next()
        })
        .and_then(|callee| qualified_or_terminal_name(callee, source))
        .map(clean_identifier)
}

fn qualified_method_invocation_name(node: TsNode, source: &[u8]) -> Option<String> {
    let object = node
        .child_by_field_name("object")
        .or_else(|| node.child_by_field_name("receiver"))?;
    let member = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("method"))
        .or_else(|| node.child_by_field_name("selector"))?;
    let object = qualified_or_terminal_name(object, source)?;
    let member = terminal_name(member, source)?;
    Some(format!("{object}.{member}"))
}

fn qualified_or_terminal_name(node: TsNode, source: &[u8]) -> Option<String> {
    if let Some(normalized) = normalized_navigation_name(node, source) {
        return Some(normalized);
    }

    if matches!(
        node.kind(),
        "member_expression"
            | "selector_expression"
            | "field_expression"
            | "navigation_expression"
            | "qualified_identifier"
            | "scoped_identifier"
    ) && let Some(raw) = text(node, source)
    {
        let compact = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
        return Some(compact);
    }

    terminal_name(node, source)
}

/// Kotlin represents both `receiver.method()` and `Constructor(...).method()`
/// as a fieldless `navigation_expression`. Its raw text contains argument
/// lists for constructor chains, which is not a stable graph target. Build the
/// target from the receiver and final member instead: `Constructor.method`.
///
/// This is intentionally lexical only. Keeping the receiver qualifier avoids
/// turning a polymorphic or inherited call into an ambiguous bare method name.
fn normalized_navigation_name(node: TsNode, source: &[u8]) -> Option<String> {
    if node.kind() != "navigation_expression" {
        return None;
    }

    let mut cursor = node.walk();
    let children = node.named_children(&mut cursor).collect::<Vec<_>>();
    let [receiver, member] = children.as_slice() else {
        return None;
    };
    let member = terminal_name(*member, source)?;
    let receiver = navigation_receiver_name(*receiver, source)?;

    if receiver.is_empty() || member.is_empty() {
        return None;
    }
    Some(format!("{receiver}.{member}"))
}

fn navigation_receiver_name(node: TsNode, source: &[u8]) -> Option<String> {
    match node.kind() {
        // These grammar nodes have no identifier child. Preserve their source
        // spelling so `super.method` and `this.method` remain qualified.
        "super_expression" | "this_expression" => text(node, source)
            .map(|raw| raw.chars().filter(|ch| !ch.is_whitespace()).collect())
            .filter(|name: &String| !name.is_empty()),
        // A constructor or factory call used as a receiver should contribute
        // its callee only; its argument text must never become part of an edge
        // target.
        "call_expression" => callee_name(node, source),
        "navigation_expression" => normalized_navigation_name(node, source),
        _ => qualified_or_terminal_name(node, source),
    }
}

fn should_skip_call(name: &str) -> bool {
    matches!(
        name,
        "if" | "for" | "while" | "switch" | "return" | "require" | "require_relative"
    )
}

fn parse_imports(language: &str, node: TsNode, source: &[u8], raw: &str) -> Vec<Import> {
    match language {
        "typescript" | "tsx" | "javascript" => {
            if let Some(import) = parse_javascript_import(node, source) {
                return vec![import];
            }
        }
        "python" if node.kind() == "import_from_statement" => {
            if let Some(import) = parse_python_from_import(node, source) {
                return vec![import];
            }
        }
        _ => {}
    }

    parse_imports_from_text(language, raw)
}

fn parse_imports_from_text(language: &str, raw: &str) -> Vec<Import> {
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

/// Parse ECMAScript named imports through the grammar's `source` field and
/// `import_clause > named_imports > import_specifier` shape. Default and
/// namespace imports have no `named_imports` child, so they deliberately keep
/// an empty symbol list.
fn parse_javascript_import(node: TsNode, source: &[u8]) -> Option<Import> {
    let source_node = node.child_by_field_name("source")?;
    let path = import_string_contents(&text(source_node, source)?);
    if path.is_empty() {
        return None;
    }

    Some(import_with_symbols(
        path,
        javascript_named_import_symbols(node, source),
    ))
}

fn javascript_named_import_symbols(node: TsNode, source: &[u8]) -> Vec<String> {
    let mut symbols = Vec::new();
    let mut statement_cursor = node.walk();
    for clause in node
        .named_children(&mut statement_cursor)
        .filter(|child| child.kind() == "import_clause")
    {
        let mut clause_cursor = clause.walk();
        for named_imports in clause
            .named_children(&mut clause_cursor)
            .filter(|child| child.kind() == "named_imports")
        {
            let mut named_imports_cursor = named_imports.walk();
            for specifier in named_imports
                .named_children(&mut named_imports_cursor)
                .filter(|child| child.kind() == "import_specifier")
            {
                if let Some(symbol) = javascript_import_symbol(specifier, source) {
                    symbols.push(symbol);
                }
            }
        }
    }
    symbols
}

fn javascript_import_symbol(specifier: TsNode, source: &[u8]) -> Option<String> {
    let name =
        text(specifier.child_by_field_name("name")?, source).map(|name| name.trim().to_string())?;
    // `import { default as Local }` is still a default import, not evidence
    // that a normal named symbol called `default` is available.
    if name.is_empty() || name == "default" {
        return None;
    }

    let alias = specifier
        .child_by_field_name("alias")
        .and_then(|alias| text(alias, source))
        .map(|alias| alias.trim().to_string())
        .filter(|alias| !alias.is_empty());
    Some(match alias {
        Some(alias) => format!("{name} as {alias}"),
        None => name,
    })
}

/// Parse `from module import name` using the Python grammar's `module_name`
/// and repeated `name` fields. Wildcard imports have no `name` fields and are
/// intentionally represented as a symbol-free module import.
fn parse_python_from_import(node: TsNode, source: &[u8]) -> Option<Import> {
    let path = text(node.child_by_field_name("module_name")?, source)
        .map(|path| path.trim().to_string())?;
    if path.is_empty() {
        return None;
    }

    let mut cursor = node.walk();
    let symbols = node
        .children_by_field_name("name", &mut cursor)
        .filter_map(|name| python_from_import_symbol(name, source))
        .collect();
    Some(import_with_symbols(path, symbols))
}

fn python_from_import_symbol(name: TsNode, source: &[u8]) -> Option<String> {
    let imported = match name.kind() {
        "dotted_name" => text(name, source).map(|name| name.trim().to_string()),
        "aliased_import" => {
            text(name.child_by_field_name("name")?, source).map(|name| name.trim().to_string())
        }
        _ => None,
    }?;
    if imported.is_empty() {
        return None;
    }

    let alias = name
        .child_by_field_name("alias")
        .and_then(|alias| text(alias, source))
        .map(|alias| alias.trim().to_string())
        .filter(|alias| !alias.is_empty());
    Some(match alias {
        Some(alias) => format!("{imported} as {alias}"),
        None => imported,
    })
}

fn import_string_contents(raw: &str) -> String {
    raw.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn import_with_symbols(path: String, symbols: Vec<String>) -> Import {
    let mut import = module_import(path);
    // Preserve the established relative-path classification. For non-relative
    // paths, a proven symbol list is the data model's `Named` import form.
    if !symbols.is_empty() && import.kind == ImportKind::Module {
        import.kind = ImportKind::Named;
    }
    import.symbols = symbols;
    import
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const EMPTY: &[&str] = &[];

    fn kotlin_language() -> tree_sitter::Language {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    static KOTLIN_CONFIG: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
        id: "kotlin",
        language: kotlin_language,
        function_types: &["function_declaration"],
        class_types: &["class_declaration"],
        method_types: &["function_declaration", "secondary_constructor"],
        interface_types: EMPTY,
        interface_kind: NodeKind::Trait,
        struct_types: EMPTY,
        enum_types: EMPTY,
        enum_member_types: &["enum_entry"],
        type_alias_types: &["type_alias"],
        import_types: &["import"],
        call_types: &["call_expression"],
        variable_types: &["property_declaration"],
        field_types: &["property_declaration"],
        property_types: EMPTY,
        extra_class_types: &["object_declaration", "companion_object"],
        name_field: "name",
        body_field: "body",
        methods_are_top_level: true,
    };

    fn extract_kotlin(source: &str) -> ExtractionResult {
        GenericTreeSitterExtractor {
            config: &KOTLIN_CONFIG,
        }
        .extract(source.as_bytes(), Path::new("Screen.kt"))
        .expect("Kotlin source should extract")
    }

    fn kotlin_field_declared_type<'a>(
        result: &'a ExtractionResult,
        field_name: &str,
    ) -> Option<&'a str> {
        result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Field && node.name == field_name)
            .and_then(|node| node.metadata.get("grapha.declared_type"))
            .map(String::as_str)
    }

    #[test]
    fn kotlin_androidx_view_model_delegates_emit_declared_types() {
        let result = extract_kotlin(
            r#"
            import androidx.fragment.app.viewModels
            import androidx.fragment.app.activityViewModels

            class Screen {
                val screenModel by viewModels<ScreenViewModel>()
                val hostModel by activityViewModels<HostViewModel>()
            }
            "#,
        );

        assert!(
            result
                .imports
                .iter()
                .any(|import| import.path == "androidx.fragment.app.viewModels")
        );
        assert!(
            result
                .imports
                .iter()
                .any(|import| import.path == "androidx.fragment.app.activityViewModels")
        );
        assert_eq!(
            kotlin_field_declared_type(&result, "screenModel"),
            Some("ScreenViewModel")
        );
        assert_eq!(
            kotlin_field_declared_type(&result, "hostModel"),
            Some("HostViewModel")
        );
    }

    #[test]
    fn kotlin_explicit_property_types_take_priority_over_delegates() {
        let result = extract_kotlin(
            r#"
            import androidx.fragment.app.viewModels

            class Screen {
                val priority: ExplicitViewModel by viewModels<DelegateViewModel>()
                val nullable: NullableViewModel? by viewModels<DelegateViewModel>()
                val unsupported: Outer.ExplicitViewModel by viewModels<DelegateViewModel>()
            }
            "#,
        );

        assert_eq!(
            kotlin_field_declared_type(&result, "priority"),
            Some("ExplicitViewModel")
        );
        assert_eq!(
            kotlin_field_declared_type(&result, "nullable"),
            Some("NullableViewModel")
        );
        assert_eq!(kotlin_field_declared_type(&result, "unsupported"), None);
    }

    #[test]
    fn kotlin_view_model_delegate_metadata_rejects_unproven_forms() {
        let unimported = extract_kotlin(
            r#"
            class Screen {
                val missingImport by viewModels<MissingImportViewModel>()
            }
            "#,
        );
        assert_eq!(
            kotlin_field_declared_type(&unimported, "missingImport"),
            None
        );

        let result = extract_kotlin(
            r#"
            import androidx.fragment.app.viewModels

            class Screen {
                val fast by fastLazy<FastViewModel>()
                val nested by viewModels<Outer.NestedViewModel>()
                val qualified by viewModels<external.QualifiedViewModel>()
                val generic by viewModels<GenericViewModel<String>>()
                val nullableDelegate by viewModels<NullableViewModel?>()
                val variance by viewModels<out VariantViewModel>()
                val wildcard by viewModels<*>()
                val chained by factory().viewModels<ChainedViewModel>()
            }
            "#,
        );

        for field in [
            "fast",
            "nested",
            "qualified",
            "generic",
            "nullableDelegate",
            "variance",
            "wildcard",
            "chained",
        ] {
            assert_eq!(
                kotlin_field_declared_type(&result, field),
                None,
                "{field} must not gain inferred metadata"
            );
        }

        let non_exact_imports = extract_kotlin(
            r#"
            import androidx.fragment.app.viewModels as fragmentViewModels
            import androidx.fragment.app.*

            class Screen {
                val aliasImport by viewModels<AliasImportViewModel>()
                val wildcardImport by viewModels<WildcardImportViewModel>()
            }
            "#,
        );
        assert_eq!(
            kotlin_field_declared_type(&non_exact_imports, "aliasImport"),
            None
        );
        assert_eq!(
            kotlin_field_declared_type(&non_exact_imports, "wildcardImport"),
            None
        );
    }

    #[test]
    fn normalizes_kotlin_constructor_chain_call_targets() {
        let result = extract_kotlin(
            r#"
            class Screen {
                fun initComponents() {
                    ChooseBetComp(this, binding).attach()
                }
            }
            "#,
        );

        let target = result
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Calls && edge.target.ends_with(".attach"))
            .map(|edge| edge.target.as_str())
            .expect("constructor-chain call edge");

        assert_eq!(target, "ChooseBetComp.attach");
        assert!(!target.contains('('));
    }

    #[test]
    fn keeps_kotlin_super_calls_qualified() {
        let result = extract_kotlin(
            r#"
            open class Parent {
                open fun initComponents() {}
            }

            class Screen : Parent() {
                fun setup() {
                    super.initComponents()
                }
            }
            "#,
        );

        let targets = result
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .map(|edge| edge.target.as_str())
            .collect::<Vec<_>>();

        assert!(targets.contains(&"super.initComponents"));
        assert!(!targets.contains(&"initComponents"));
    }
}
