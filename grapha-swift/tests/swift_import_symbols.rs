use std::path::Path;

use grapha_core::resolve::{Import, ImportKind};
use grapha_swift::extract_swift_via_fallback_for_tests;

fn fallback_imports(source: &str) -> Vec<Import> {
    extract_swift_via_fallback_for_tests(source.as_bytes(), Path::new("imports.swift"))
        .expect("Swift tree-sitter fallback should extract imports")
        .imports
}

fn assert_swift_grammar_accepts(source: &str) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("Swift grammar should load");
    let tree = parser
        .parse(source, None)
        .expect("Swift grammar should produce a tree");
    assert!(
        !tree.root_node().has_error(),
        "the import forms under test must be accepted by the Swift tree-sitter grammar: {}",
        tree.root_node().to_sexp()
    );
}

fn import(path: &str, symbols: &[&str], kind: ImportKind) -> Import {
    Import {
        path: path.to_string(),
        symbols: symbols.iter().map(|symbol| (*symbol).to_string()).collect(),
        kind,
    }
}

#[test]
fn fallback_preserves_bare_and_untyped_dotted_imports_as_modules() {
    let source = r#"
        import Foundation
        import VendorKit.Submodule
        "#;
    assert_swift_grammar_accepts(source);
    let imports = fallback_imports(source);

    assert_eq!(
        imports,
        vec![
            import("Foundation", &[], ImportKind::Module),
            import("VendorKit.Submodule", &[], ImportKind::Module),
        ],
        "bare and untyped dotted import paths must not invent imported symbols"
    );
}

#[test]
fn fallback_extracts_explicit_declaration_imports_as_named_symbols() {
    let source = r#"
        import typealias Models.RecordAlias
        import struct Models.Record
        import struct Models.Storage.StoredRecord
        import class Models.Controller
        import enum Models.Mode
        import protocol Models.Runnable
        import let Constants.defaultValue
        import var State.sharedValue
        import func Helpers.makeValue
        "#;
    assert_swift_grammar_accepts(source);
    let imports = fallback_imports(source);

    assert_eq!(
        imports,
        vec![
            import("Models", &["RecordAlias"], ImportKind::Named),
            import("Models", &["Record"], ImportKind::Named),
            import("Models.Storage", &["StoredRecord"], ImportKind::Named),
            import("Models", &["Controller"], ImportKind::Named),
            import("Models", &["Mode"], ImportKind::Named),
            import("Models", &["Runnable"], ImportKind::Named),
            import("Constants", &["defaultValue"], ImportKind::Named),
            import("State", &["sharedValue"], ImportKind::Named),
            import("Helpers", &["makeValue"], ImportKind::Named),
        ],
        "each explicit Swift declaration import should retain its module and bound symbol"
    );
}
