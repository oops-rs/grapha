use grapha_core::graph::NodeKind;
use grapha_core::{
    ExtractionResult, FileContext, GenericTreeSitterExtractor, LanguageExtractor, LanguagePlugin,
    LanguageRegistry, TreeSitterLanguageConfig,
};

pub struct PolyglotPlugin;

impl LanguagePlugin for PolyglotPlugin {
    fn id(&self) -> &'static str {
        "polyglot-tree-sitter"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[
            "ts", "tsx", "js", "mjs", "cjs", "jsx", "py", "pyw", "go", "java", "c", "h", "cpp",
            "cc", "cxx", "hpp", "hxx", "cs", "php", "rb", "rake", "kt", "kts", "dart", "pas",
            "dpr", "dpk", "lpr",
        ]
    }

    fn extract(&self, source: &[u8], context: &FileContext) -> anyhow::Result<ExtractionResult> {
        let config = config_for_path(&context.relative_path, source).ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported best-effort tree-sitter language: {}",
                context.relative_path.display()
            )
        })?;
        let extractor = GenericTreeSitterExtractor { config };
        extractor.extract(source, &context.relative_path)
    }
}

pub fn register_builtin(registry: &mut LanguageRegistry) -> anyhow::Result<()> {
    registry.register(PolyglotPlugin)
}

fn config_for_path(
    path: &std::path::Path,
    source: &[u8],
) -> Option<&'static TreeSitterLanguageConfig> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "ts" => Some(&TYPESCRIPT),
        "tsx" => Some(&TSX),
        "js" | "mjs" | "cjs" | "jsx" => Some(&JAVASCRIPT),
        "py" | "pyw" => Some(&PYTHON),
        "go" => Some(&GO),
        "java" => Some(&JAVA),
        "c" => Some(&C),
        "h" if looks_like_cpp(source) => Some(&CPP),
        "h" => Some(&C),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(&CPP),
        "cs" => Some(&CSHARP),
        "php" => Some(&PHP),
        "rb" | "rake" => Some(&RUBY),
        "kt" | "kts" => Some(&KOTLIN),
        "dart" => Some(&DART),
        "pas" | "dpr" | "dpk" | "lpr" => Some(&PASCAL),
        _ => None,
    }
}

fn looks_like_cpp(source: &[u8]) -> bool {
    let sample_len = source.len().min(8192);
    let sample = String::from_utf8_lossy(&source[..sample_len]);
    sample.contains("namespace ")
        || sample.contains("template <")
        || sample.contains("template<")
        || sample.contains("class ")
        || sample.contains("public:")
        || sample.contains("private:")
        || sample.contains("protected:")
        || sample.contains("virtual ")
        || sample.contains("using namespace")
}

fn ts_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn tsx_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

fn javascript_language() -> tree_sitter::Language {
    tree_sitter_javascript::LANGUAGE.into()
}

fn python_language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

fn go_language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

fn java_language() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}

fn c_language() -> tree_sitter::Language {
    tree_sitter_c::LANGUAGE.into()
}

fn cpp_language() -> tree_sitter::Language {
    tree_sitter_cpp::LANGUAGE.into()
}

fn csharp_language() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

fn php_language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

fn ruby_language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}

fn kotlin_language() -> tree_sitter::Language {
    tree_sitter_kotlin_ng::LANGUAGE.into()
}

fn dart_language() -> tree_sitter::Language {
    tree_sitter_dart::LANGUAGE.into()
}

fn pascal_language() -> tree_sitter::Language {
    tree_sitter_pascal::LANGUAGE.into()
}

const EMPTY: &[&str] = &[];

const TYPESCRIPT: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "typescript",
    language: ts_language,
    function_types: &[
        "function_declaration",
        "arrow_function",
        "function_expression",
    ],
    class_types: &["class_declaration", "abstract_class_declaration"],
    method_types: &["method_definition", "public_field_definition"],
    interface_types: &["interface_declaration"],
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: &["enum_declaration"],
    enum_member_types: EMPTY,
    type_alias_types: &["type_alias_declaration"],
    import_types: &["import_statement"],
    call_types: &["call_expression"],
    variable_types: &["lexical_declaration", "variable_declaration"],
    field_types: &["public_field_definition"],
    property_types: EMPTY,
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const TSX: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "tsx",
    language: tsx_language,
    ..TYPESCRIPT
};

const JAVASCRIPT: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "javascript",
    language: javascript_language,
    interface_types: EMPTY,
    enum_types: EMPTY,
    type_alias_types: EMPTY,
    ..TYPESCRIPT
};

const PYTHON: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "python",
    language: python_language,
    function_types: &["function_definition"],
    class_types: &["class_definition"],
    method_types: &["function_definition"],
    interface_types: EMPTY,
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: EMPTY,
    enum_member_types: EMPTY,
    type_alias_types: EMPTY,
    import_types: &["import_statement", "import_from_statement"],
    call_types: &["call"],
    variable_types: &["assignment"],
    field_types: EMPTY,
    property_types: EMPTY,
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const GO: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "go",
    language: go_language,
    function_types: &["function_declaration"],
    class_types: EMPTY,
    method_types: &["method_declaration"],
    interface_types: EMPTY,
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: EMPTY,
    enum_member_types: EMPTY,
    type_alias_types: &["type_spec"],
    import_types: &["import_declaration"],
    call_types: &["call_expression"],
    variable_types: &[
        "var_declaration",
        "short_var_declaration",
        "const_declaration",
    ],
    field_types: EMPTY,
    property_types: EMPTY,
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: true,
};

const JAVA: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "java",
    language: java_language,
    function_types: EMPTY,
    class_types: &["class_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    interface_types: &["interface_declaration"],
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: &["enum_declaration"],
    enum_member_types: EMPTY,
    type_alias_types: EMPTY,
    import_types: &["import_declaration"],
    call_types: &["method_invocation"],
    variable_types: &["local_variable_declaration"],
    field_types: &["field_declaration"],
    property_types: EMPTY,
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const C: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "c",
    language: c_language,
    function_types: &["function_definition"],
    class_types: EMPTY,
    method_types: EMPTY,
    interface_types: EMPTY,
    interface_kind: NodeKind::Trait,
    struct_types: &["struct_specifier"],
    enum_types: &["enum_specifier"],
    enum_member_types: &["enumerator"],
    type_alias_types: &["type_definition"],
    import_types: &["preproc_include"],
    call_types: &["call_expression"],
    variable_types: &["declaration"],
    field_types: &["field_declaration"],
    property_types: EMPTY,
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const CPP: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "cpp",
    language: cpp_language,
    class_types: &["class_specifier"],
    method_types: &["function_definition"],
    type_alias_types: &["type_definition", "alias_declaration"],
    ..C
};

const CSHARP: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "csharp",
    language: csharp_language,
    function_types: EMPTY,
    class_types: &["class_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    interface_types: &["interface_declaration"],
    interface_kind: NodeKind::Trait,
    struct_types: &["struct_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: EMPTY,
    type_alias_types: EMPTY,
    import_types: &["using_directive"],
    call_types: &["invocation_expression"],
    variable_types: &["local_declaration_statement"],
    field_types: &["field_declaration"],
    property_types: &["property_declaration"],
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const PHP: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "php",
    language: php_language,
    function_types: &["function_definition"],
    class_types: &["class_declaration", "trait_declaration"],
    method_types: &["method_declaration"],
    interface_types: &["interface_declaration"],
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: &["enum_declaration"],
    enum_member_types: EMPTY,
    type_alias_types: EMPTY,
    import_types: &["namespace_use_declaration"],
    call_types: &[
        "function_call_expression",
        "member_call_expression",
        "scoped_call_expression",
    ],
    variable_types: &["const_declaration"],
    field_types: &["property_declaration"],
    property_types: EMPTY,
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const RUBY: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "ruby",
    language: ruby_language,
    function_types: &["method"],
    class_types: &["class"],
    method_types: &["method", "singleton_method"],
    interface_types: EMPTY,
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: EMPTY,
    enum_member_types: EMPTY,
    type_alias_types: EMPTY,
    import_types: &["call"],
    call_types: &["call", "method_call"],
    variable_types: &["assignment"],
    field_types: EMPTY,
    property_types: EMPTY,
    extra_class_types: &["module"],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const KOTLIN: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "kotlin",
    language: kotlin_language,
    function_types: &["function_declaration"],
    class_types: &["class_declaration"],
    method_types: &["function_declaration"],
    interface_types: EMPTY,
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: EMPTY,
    enum_member_types: EMPTY,
    type_alias_types: &["type_alias"],
    import_types: &["import_header"],
    call_types: &["call_expression"],
    variable_types: &["property_declaration"],
    field_types: &["property_declaration"],
    property_types: EMPTY,
    extra_class_types: &["object_declaration"],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const DART: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "dart",
    language: dart_language,
    function_types: &["function_declaration"],
    class_types: &["class_declaration"],
    method_types: &["method_declaration", "method_signature"],
    interface_types: EMPTY,
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: &["enum_declaration"],
    enum_member_types: EMPTY,
    type_alias_types: &["type_alias"],
    import_types: &["import_or_export"],
    call_types: EMPTY,
    variable_types: EMPTY,
    field_types: EMPTY,
    property_types: EMPTY,
    extra_class_types: &[
        "mixin_declaration",
        "extension_declaration",
        "extension_type_declaration",
    ],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

const PASCAL: TreeSitterLanguageConfig = TreeSitterLanguageConfig {
    id: "pascal",
    language: pascal_language,
    function_types: &["declProc"],
    class_types: &["declClass"],
    method_types: &["declProc"],
    interface_types: &["declIntf"],
    interface_kind: NodeKind::Trait,
    struct_types: EMPTY,
    enum_types: &["declEnum"],
    enum_member_types: EMPTY,
    type_alias_types: &["declType"],
    import_types: &["declUses"],
    call_types: &["exprCall"],
    variable_types: &["declField", "declConst"],
    field_types: &["declField"],
    property_types: &["declProperty"],
    extra_class_types: EMPTY,
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_h_header_to_cpp_when_it_uses_cpp_constructs() {
        let config = config_for_path(
            std::path::Path::new("Widget.h"),
            b"namespace app { class Widget {}; }",
        )
        .unwrap();

        assert_eq!(config.id, "cpp");
    }

    #[test]
    fn routes_h_header_to_c_by_default() {
        let config = config_for_path(std::path::Path::new("widget.h"), b"struct Widget;").unwrap();

        assert_eq!(config.id, "c");
    }
}
