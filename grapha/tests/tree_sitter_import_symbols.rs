use std::path::PathBuf;

use grapha::polyglot_plugin::PolyglotPlugin;
use grapha_core::resolve::{Import, ImportKind};
use grapha_core::{FileContext, LanguagePlugin};

fn extracted_imports(relative_path: &str, source: &str) -> Vec<Import> {
    let relative_path = PathBuf::from(relative_path);
    let context = FileContext {
        input_path: relative_path.clone(),
        project_root: PathBuf::from("."),
        relative_path: relative_path.clone(),
        absolute_path: relative_path,
        module_name: None,
        index_store_enabled: false,
    };

    PolyglotPlugin
        .extract(source.as_bytes(), &context)
        .expect("generic tree-sitter extraction should succeed")
        .imports
}

fn import(path: &str, symbols: &[&str], kind: ImportKind) -> Import {
    Import {
        path: path.to_string(),
        symbols: symbols.iter().map(|symbol| (*symbol).to_string()).collect(),
        kind,
    }
}

fn assert_grammar_accepts(language: tree_sitter::Language, source: &str) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .expect("tree-sitter grammar should load");
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter grammar should produce a tree");
    assert!(
        !tree.root_node().has_error(),
        "the import forms under test must be accepted by the grammar: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn generic_extractor_captures_named_typescript_and_javascript_imports() {
    let typescript = r#"import { alpha, beta as localBeta } from "package";"#;
    assert_grammar_accepts(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        typescript,
    );
    assert_eq!(
        extracted_imports("imports.ts", typescript),
        vec![import(
            "package",
            &["alpha", "beta as localBeta"],
            ImportKind::Named,
        )],
    );

    let javascript = r#"import { gamma, delta as localDelta } from "./helpers";"#;
    assert_grammar_accepts(tree_sitter_javascript::LANGUAGE.into(), javascript);
    assert_eq!(
        extracted_imports("imports.js", javascript),
        vec![import(
            "./helpers",
            &["gamma", "delta as localDelta"],
            ImportKind::Relative,
        )],
        "a relative source path remains relative while its named bindings are retained",
    );
}

#[test]
fn generic_extractor_captures_python_from_imports_with_the_module_path() {
    let source = "from package.submodule import Widget, render_widget as render\n";
    assert_grammar_accepts(tree_sitter_python::LANGUAGE.into(), source);

    assert_eq!(
        extracted_imports("imports.py", source),
        vec![import(
            "package.submodule",
            &["Widget", "render_widget as render"],
            ImportKind::Named,
        )],
        "the module field must not absorb the `import` clause",
    );
}

#[test]
fn generic_extractor_captures_kotlin_imports() {
    let source = "import androidx.fragment.app.viewModels\n";
    assert_grammar_accepts(tree_sitter_kotlin_ng::LANGUAGE.into(), source);

    assert_eq!(
        extracted_imports("imports.kt", source),
        vec![import(
            "androidx.fragment.app.viewModels",
            &[],
            ImportKind::Module,
        )],
        "the Kotlin grammar's `import` node must reach the generic extractor",
    );
}

#[test]
fn generic_extractor_leaves_default_wildcard_and_bare_imports_symbol_free() {
    let javascript = r#"
        import DefaultExport from "default-package";
        import * as namespace from "namespace-package";
        import "side-effect-package";
        import { default as Rebound } from "default-specifier-package";
    "#;
    assert_grammar_accepts(tree_sitter_javascript::LANGUAGE.into(), javascript);
    assert_eq!(
        extracted_imports("unsupported.js", javascript),
        vec![
            import("default-package", &[], ImportKind::Module),
            import("namespace-package", &[], ImportKind::Module),
            import("side-effect-package", &[], ImportKind::Module),
            import("default-specifier-package", &[], ImportKind::Module),
        ],
        "default, namespace, and side-effect imports do not prove named symbols",
    );

    let python = "from helpers import *\nimport os\n";
    assert_grammar_accepts(tree_sitter_python::LANGUAGE.into(), python);
    assert_eq!(
        extracted_imports("unsupported.py", python),
        vec![
            import("helpers", &[], ImportKind::Module),
            import("os", &[], ImportKind::Module),
        ],
        "wildcard and bare imports remain symbol-free",
    );
}
