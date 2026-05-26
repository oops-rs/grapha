use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use grapha_core::ExtractionResult;

use crate::bridge;

/// Try to extract Swift symbols using SwiftSyntax via the bridge's JSON-string
/// FFI path.
pub fn extract_with_swiftsyntax(source: &[u8], file_path: &Path) -> Option<ExtractionResult> {
    let bridge = bridge::bridge()?;

    let file_path_c = CString::new(file_path.to_str()?).ok()?;
    let json_ptr = unsafe {
        (bridge.swiftsyntax_extract)(
            source.as_ptr() as *const c_char,
            source.len(),
            file_path_c.as_ptr(),
        )
    };

    if json_ptr.is_null() {
        return None;
    }

    let json_bytes = unsafe { CStr::from_ptr(json_ptr) }.to_bytes().to_vec();
    unsafe { (bridge.free_string)(json_ptr as *mut c_char) };
    let result: ExtractionResult = serde_json::from_slice(&json_bytes).ok()?;

    Some(result)
}

#[cfg(all(test, not(no_swift_bridge)))]
mod tests {
    use std::path::Path;

    use grapha_core::graph::{EdgeKind, NodeKind, NodeRole};

    use super::extract_with_swiftsyntax;
    use crate::extract_swift;

    fn fixture_path() -> &'static Path {
        Path::new("test.swift")
    }

    fn find_node<'a>(
        result: &'a grapha_core::ExtractionResult,
        name: &str,
        kind: NodeKind,
    ) -> &'a grapha_core::graph::Node {
        result
            .nodes
            .iter()
            .find(|node| node.name == name && node.kind == kind)
            .unwrap_or_else(|| panic!("missing {kind:?} node {name}"))
    }

    fn has_edge(
        result: &grapha_core::ExtractionResult,
        source: &str,
        target: &str,
        kind: EdgeKind,
    ) -> bool {
        result
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == target && edge.kind == kind)
    }

    #[test]
    fn extracts_swiftsyntax_fixture_symbols_and_edges() {
        let source = include_str!("../../grapha/tests/fixtures/simple.swift");
        let result = extract_with_swiftsyntax(source.as_bytes(), fixture_path())
            .expect("SwiftSyntax extraction should succeed");

        let config = find_node(&result, "Config", NodeKind::Struct);
        let configurable = find_node(&result, "Configurable", NodeKind::Protocol);
        let app_delegate = find_node(&result, "AppDelegate", NodeKind::Class);
        let launch = find_node(&result, "launch", NodeKind::Function);
        let default_config = find_node(&result, "defaultConfig", NodeKind::Function);
        let theme = find_node(&result, "Theme", NodeKind::Enum);
        let light = find_node(&result, "light", NodeKind::Variant);

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].path, "Foundation");
        assert!(has_edge(
            &result,
            &app_delegate.id,
            "test.swift::Configurable",
            EdgeKind::Implements
        ));
        assert!(has_edge(&result, &theme.id, &light.id, EdgeKind::Contains));
        assert!(has_edge(
            &result,
            &launch.id,
            "test.swift::configure",
            EdgeKind::Calls
        ));
        assert!(
            default_config
                .signature
                .as_deref()
                .is_some_and(|signature| { signature.contains("func defaultConfig() -> Config") }),
            "SwiftSyntax should preserve function signatures"
        );
        assert_eq!(config.kind, NodeKind::Struct);
        assert_eq!(configurable.kind, NodeKind::Protocol);
    }

    #[test]
    fn extracts_swiftsyntax_extensions_and_inheritance_edges() {
        let source = r#"
        class Base {}
        protocol Runnable {}
        class Worker: Base, Runnable {}

        extension Worker {
            func helper() {}
        }
        "#;

        let result = extract_with_swiftsyntax(source.as_bytes(), fixture_path())
            .expect("SwiftSyntax extraction should succeed");

        let worker = find_node(&result, "Worker", NodeKind::Class);
        let extension_node = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Extension && node.name == "Worker")
            .expect("extension node should exist");
        let helper = find_node(&result, "helper", NodeKind::Function);

        assert!(has_edge(
            &result,
            &worker.id,
            "test.swift::Base",
            EdgeKind::Inherits
        ));
        assert!(has_edge(
            &result,
            &worker.id,
            "test.swift::Runnable",
            EdgeKind::Implements
        ));
        assert!(has_edge(
            &result,
            &extension_node.id,
            &helper.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extract_swift_runs_swiftsyntax_with_doc_and_swiftui_enrichment() {
        let source = br#"
        import SwiftUI

        struct ContentView: View {
            /// The main body.
            var body: some View {
                VStack {
                    Text("Hello")
                }
            }
        }
        "#;

        let result =
            extract_swift(source, Path::new("ContentView.swift"), None, None, true).unwrap();

        let body = result
            .nodes
            .iter()
            .find(|node| node.id == "ContentView.swift::ContentView::body")
            .expect("body property should exist");
        let vstack = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::View && node.name == "VStack")
            .expect("VStack synthetic view should exist");

        assert_eq!(body.role, Some(NodeRole::EntryPoint));
        assert!(
            body.doc_comment
                .as_deref()
                .is_some_and(|doc| doc.contains("The main body")),
            "doc comments should be enriched onto SwiftSyntax output"
        );
        assert!(has_edge(&result, &body.id, &vstack.id, EdgeKind::Contains));
    }
}
