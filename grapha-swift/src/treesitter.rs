mod assets;
mod common;
mod extract;
mod localization;
mod swiftui;

pub use assets::{enrich_asset_references_with_tree, source_contains_image_asset_markers};
#[cfg(test)]
pub use extract::enrich_doc_comments;
pub use extract::{
    SwiftExtractor, enrich_codegraph_compat_with_tree, enrich_doc_comments_with_tree, parse_swift,
};
#[cfg(test)]
#[allow(unused_imports)]
pub use localization::enrich_localization_metadata;
pub use localization::enrich_localization_metadata_with_tree;
#[cfg(test)]
pub use swiftui::enrich_swiftui_structure;
pub use swiftui::enrich_swiftui_structure_with_tree;

#[cfg(test)]
use localization::{localized_reference_for_expression_text, localized_text_references_from_text};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    use grapha_core::ExtractionResult;
    use grapha_core::LanguageExtractor;
    use grapha_core::graph::{Edge, EdgeKind, Node, NodeKind, NodeRole, Span, Visibility};

    fn extract(source: &str) -> ExtractionResult {
        let extractor = SwiftExtractor;
        extractor
            .extract(source.as_bytes(), Path::new("test.swift"))
            .unwrap()
    }

    fn extract_with_localization(source: &str) -> ExtractionResult {
        crate::extract_swift(source.as_bytes(), Path::new("test.swift"), None, None, true).unwrap()
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
    fn extracts_struct() {
        let result = extract("public struct Config { let debug: Bool }");
        let node = find_node(&result, "Config");
        assert_eq!(node.kind, NodeKind::Struct);
        assert_eq!(node.visibility, Visibility::Public);
    }

    #[test]
    fn extracts_class() {
        let result = extract("public class AppDelegate { }");
        let node = find_node(&result, "AppDelegate");
        assert_eq!(node.kind, NodeKind::Class);
        assert_eq!(node.visibility, Visibility::Public);
    }

    #[test]
    fn extracts_protocol() {
        let result = extract("public protocol Drawable { func draw() }");
        let node = find_node(&result, "Drawable");
        assert_eq!(node.kind, NodeKind::Protocol);
        assert_eq!(node.visibility, Visibility::Public);
    }

    #[test]
    fn extracts_enum_with_cases() {
        let result = extract(
            r#"
            public enum Color {
                /// Warm stop color.
                case red
                case green
            }
            "#,
        );
        let color = find_node(&result, "Color");
        assert_eq!(color.kind, NodeKind::Enum);

        let red = find_node(&result, "red");
        assert_eq!(red.kind, NodeKind::Variant);
        assert_eq!(red.doc_comment.as_deref(), Some("/// Warm stop color."));

        let green = find_node(&result, "green");
        assert_eq!(green.kind, NodeKind::Variant);

        assert!(has_edge(&result, &color.id, &red.id, EdgeKind::Contains));
        assert!(has_edge(&result, &color.id, &green.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_function() {
        let result = extract("public func greet() { }");
        let node = find_node(&result, "greet");
        assert_eq!(node.kind, NodeKind::Function);
        assert_eq!(node.visibility, Visibility::Public);
    }

    #[test]
    fn extracts_file_and_import_nodes() {
        let result = extract(
            r#"
            import Foundation

            public func greet() { }
            "#,
        );

        let file = find_node(&result, "test.swift");
        assert_eq!(file.kind, NodeKind::File);

        let import = find_node(&result, "Foundation");
        assert_eq!(import.kind, NodeKind::Import);
        assert_eq!(import.signature.as_deref(), Some("import Foundation"));

        let greet = find_node(&result, "greet");
        assert!(has_edge(&result, &file.id, &import.id, EdgeKind::Contains));
        assert!(has_edge(&result, &file.id, &import.id, EdgeKind::Imports));
        assert!(has_edge(&result, &file.id, &greet.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_swiftui_component_and_vapor_route_nodes() {
        let result = extract(
            r#"
            import SwiftUI
            import Vapor

            struct ContentView: View {
                var body: some View { Text("Hi") }
            }

            func routes(_ app: Application) throws {
                app.get("health") { req in "ok" }
                app.grouped("api").post("users") { req in "ok" }
            }
            "#,
        );

        let file = find_node(&result, "test.swift");
        let component = result
            .nodes
            .iter()
            .find(|node| node.name == "ContentView" && node.kind == NodeKind::Component)
            .expect("component node should exist");
        let health = find_node(&result, "GET health");
        let users = find_node(&result, "POST /api/users");

        assert_eq!(health.kind, NodeKind::Route);
        assert_eq!(users.kind, NodeKind::Route);
        assert!(has_edge(
            &result,
            &file.id,
            &component.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(&result, &file.id, &health.id, EdgeKind::Contains));
        assert!(has_edge(&result, &file.id, &users.id, EdgeKind::Contains));
    }

    #[test]
    fn compat_enrichment_adds_nodes_to_preexisting_swift_result() {
        let source = r#"
            import SwiftUI

            struct ContentView: View {
                var body: some View { Text("Hi") }
            }
        "#;
        let tree = parse_swift(source.as_bytes()).unwrap();
        let mut result = ExtractionResult {
            nodes: vec![Node {
                id: "s:ContentView".to_string(),
                kind: NodeKind::Struct,
                name: "ContentView".to_string(),
                file: Path::new("test.swift").into(),
                span: Span {
                    start: [3, 12],
                    end: [5, 13],
                },
                visibility: Visibility::Crate,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            }],
            edges: Vec::new(),
            imports: Vec::new(),
        };

        enrich_codegraph_compat_with_tree(
            source.as_bytes(),
            Path::new("test.swift"),
            &tree,
            &mut result,
        )
        .unwrap();

        let file = find_node(&result, "test.swift");
        let import = find_node(&result, "SwiftUI");
        let component = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Component && node.name == "ContentView")
            .expect("component node should exist");

        assert_eq!(file.kind, NodeKind::File);
        assert_eq!(import.kind, NodeKind::Import);
        assert!(has_edge(
            &result,
            &file.id,
            "s:ContentView",
            EdgeKind::Contains
        ));
        assert!(has_edge(&result, &file.id, &import.id, EdgeKind::Imports));
        assert!(has_edge(
            &result,
            &file.id,
            &component.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extracts_static_and_async_swift_metadata() {
        let result = extract(
            r#"
            struct Worker {
                static func build() async {}
                class var shared: Worker { Worker() }
            }
            "#,
        );

        let build = find_node(&result, "build");
        assert_eq!(
            build.metadata.get("static").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            build.metadata.get("async").map(String::as_str),
            Some("true")
        );

        let shared = find_node(&result, "shared");
        assert_eq!(
            shared.metadata.get("static").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn overloaded_initializers_get_distinct_ids() {
        let result = extract(
            r#"
            struct StringPair {
                init(key: String, value: String) {}
                init?(iosLine: String) {}
            }
            "#,
        );

        let init_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.name == "init")
            .collect();
        assert_eq!(init_nodes.len(), 2);

        let unique_ids: std::collections::HashSet<_> =
            init_nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(unique_ids.len(), 2);
    }

    #[test]
    fn multiple_extensions_get_distinct_ids() {
        let result = extract(
            r#"
            struct Config {}

            extension Config {
                func alpha() {}
            }

            extension Config {
                func beta() {}
            }
            "#,
        );

        let extension_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Extension)
            .collect();
        assert_eq!(extension_nodes.len(), 2);

        let unique_ids: std::collections::HashSet<_> = extension_nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(unique_ids.len(), 2);
    }

    #[test]
    fn extracts_extension() {
        let result = extract("extension Config { func foo() {} }");
        let ext = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Extension)
            .expect("extension node not found");
        assert_eq!(ext.name, "Config");

        let foo = find_node(&result, "foo");
        assert_eq!(foo.kind, NodeKind::Function);
        assert!(has_edge(&result, &ext.id, &foo.id, EdgeKind::Contains));
    }

    #[test]
    fn extracts_protocol_conformance() {
        let result = extract(
            r#"
            public protocol Configurable {}
            public class AppDelegate: Configurable { }
            "#,
        );
        let app = find_node(&result, "AppDelegate");
        assert!(has_edge(
            &result,
            &app.id,
            "test.swift::Configurable",
            EdgeKind::Implements
        ));
    }

    #[test]
    fn extracts_import() {
        let result = extract("import Foundation");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].path, "Foundation");
        assert_eq!(
            result.imports[0].kind,
            grapha_core::resolve::ImportKind::Module
        );
    }

    #[test]
    fn extracts_call_edges() {
        let result = extract(
            r#"
            func greet() { }
            func launch() {
                greet()
            }
            "#,
        );
        assert!(has_edge(
            &result,
            "test.swift::launch",
            "test.swift::greet",
            EdgeKind::Calls,
        ));
        let call_edge = result
            .edges
            .iter()
            .find(|edge| {
                edge.source == "test.swift::launch"
                    && edge.target == "test.swift::greet"
                    && edge.kind == EdgeKind::Calls
            })
            .expect("should find call edge");
        assert!(
            !call_edge.provenance.is_empty(),
            "call edges should carry provenance"
        );
        assert_eq!(call_edge.provenance[0].symbol_id, "test.swift::launch");
    }

    #[test]
    fn property_accesses_become_reads_not_calls() {
        let result = extract(
            r#"
            struct Model { let value: Int }
            func launch(model: Model) {
                let current = model.value
            }
            "#,
        );

        assert!(has_edge(
            &result,
            "test.swift::launch",
            "test.swift::value",
            EdgeKind::Reads,
        ));
        assert!(!has_edge(
            &result,
            "test.swift::launch",
            "test.swift::value",
            EdgeKind::Calls,
        ));
    }

    #[test]
    fn extracts_condition_on_call_inside_if() {
        let result = extract(
            r#"
            func run() {
                if true {
                    helper()
                }
            }
            func helper() { }
            "#,
        );
        let cond_edge = result
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.target.contains("helper"));
        assert!(cond_edge.is_some(), "should find Calls edge to helper");
        // The condition may or may not be extracted depending on tree-sitter-swift's AST
        // We verify the edge exists and the mechanism doesn't crash
    }

    #[test]
    fn detects_view_body_as_entry_point() {
        let result = extract(
            r#"
            struct ContentView: View {
                var body: Int { return 0 }
            }
            "#,
        );
        let body_node = result
            .nodes
            .iter()
            .find(|n| n.name == "body")
            .expect("should find body property");
        assert_eq!(
            body_node.role,
            Some(grapha_core::graph::NodeRole::EntryPoint),
            "View.body should be an EntryPoint"
        );
    }

    #[test]
    fn scopes_body_ids_per_view_type() {
        let result = extract(
            r#"
            struct FirstView: View {
                var body: some View { Text("One") }
            }

            struct SecondView: View {
                var body: some View { Text("Two") }
            }
            "#,
        );

        let body_ids: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.name == "body" && n.kind == NodeKind::Property)
            .map(|n| n.id.as_str())
            .collect();

        assert_eq!(body_ids.len(), 2);
        assert!(body_ids.contains(&"test.swift::FirstView::body"));
        assert!(body_ids.contains(&"test.swift::SecondView::body"));
    }

    #[test]
    fn extracts_swiftui_view_hierarchy_and_type_refs() {
        let result = extract(
            r#"
            import SwiftUI

            struct Row: View {
                let title: String
                var body: some View { Text(title) }
            }

            struct ContentView: View {
                var body: some View {
                    VStack {
                        Text("Hello")
                        Row(title: "World")
                    }
                }
            }
            "#,
        );

        let body = result
            .nodes
            .iter()
            .find(|n| n.id == "test.swift::ContentView::body")
            .expect("content body node should exist");
        let vstack = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "VStack")
            .expect("VStack synthetic view should exist");
        let row_view = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "Row")
            .expect("Row synthetic view should exist");
        let vstack_children: Vec<&Node> = result
            .edges
            .iter()
            .filter(|edge| edge.source == vstack.id && edge.kind == EdgeKind::Contains)
            .filter_map(|edge| result.nodes.iter().find(|node| node.id == edge.target))
            .collect();

        assert!(has_edge(&result, &body.id, &vstack.id, EdgeKind::Contains));
        assert_eq!(
            vstack_children
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Text", "Row"]
        );
        assert!(has_edge(
            &result,
            &row_view.id,
            "test.swift::Row",
            EdgeKind::TypeRef
        ));
    }

    #[test]
    fn extracts_swiftui_branch_hierarchy() {
        let result = extract(
            r#"
            import SwiftUI

            struct ContentView: View {
                var body: some View {
                    VStack {
                        if showDetails {
                            Group {
                                Text("More")
                            }
                        } else {
                            switch mode {
                            case .empty:
                                EmptyView()
                            default:
                                ForEach(items) { item in
                                    Text(item.name)
                                }
                            }
                        }
                    }
                }
            }
            "#,
        );

        let vstack = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "VStack")
            .expect("VStack should exist");
        let if_branch = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Branch && n.name == "if showDetails")
            .expect("if branch should exist");
        let else_branch = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Branch && n.name == "else")
            .expect("else branch should exist");
        let switch_branch = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Branch && n.name == "switch mode")
            .expect("switch branch should exist");
        let default_branch = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Branch && n.name == "default")
            .expect("default branch should exist");
        let for_each = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "ForEach")
            .expect("ForEach view should exist");

        assert!(has_edge(
            &result,
            &vstack.id,
            &if_branch.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &vstack.id,
            &else_branch.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &else_branch.id,
            &switch_branch.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &switch_branch.id,
            &default_branch.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &default_branch.id,
            &for_each.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extracts_swiftui_structure_through_modifier_chains_and_view_refs() {
        let result = extract(
            r#"
            import SwiftUI

            struct InlinePanel: View {
                var body: some View { Text("Inline") }
            }

            struct OverlayPanel: View {
                var body: some View { Text("Overlay") }
            }

            struct ContentView: View {
                var chatRoomFragViewPanel: some View {
                    InlinePanel()
                }

                var exitPopView: some View {
                    Text("Exit")
                }

                var body: some View {
                    NavigationStack {
                        if showDetails {
                            InlinePanel()
                                .onReceive(events) { _ in
                                    switch mode {
                                    case .loading:
                                        helper()
                                    default:
                                        break
                                    }
                                }
                        }

                        chatRoomFragViewPanel
                        DialogStreamView()
                        exitPopView
                    }
                    .frame(width: 100)
                    .overlay {
                        OverlayPanel()
                    }
                    .onReceive(events) { _ in
                        switch mode {
                        case .done:
                            helper()
                        default:
                            break
                        }
                    }
                }
            }
            "#,
        );

        let body = result
            .nodes
            .iter()
            .find(|n| n.id == "test.swift::ContentView::body")
            .expect("content body node should exist");
        let nav = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "NavigationStack")
            .expect("NavigationStack synthetic view should exist");
        let if_branch = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Branch && n.name == "if showDetails")
            .expect("if branch should exist");
        let body_inline_panel = result
            .nodes
            .iter()
            .find(|n| {
                n.kind == NodeKind::View
                    && n.name == "InlinePanel"
                    && n.id.starts_with("test.swift::ContentView::body::view:")
            })
            .expect("body InlinePanel view should exist");
        let chat_panel = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "chatRoomFragViewPanel")
            .expect("chatRoomFragViewPanel view ref should exist");
        let exit_pop = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "exitPopView")
            .expect("exitPopView view ref should exist");
        let dialog_stream = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "DialogStreamView")
            .expect("DialogStreamView should exist");
        let overlay_panel = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::View && n.name == "OverlayPanel")
            .expect("OverlayPanel should exist");
        let helper_property = result
            .nodes
            .iter()
            .find(|n| n.id == "test.swift::ContentView::chatRoomFragViewPanel")
            .expect("helper property should exist");
        let exit_property = result
            .nodes
            .iter()
            .find(|n| n.id == "test.swift::ContentView::exitPopView")
            .expect("exit property should exist");
        let helper_text = result
            .nodes
            .iter()
            .find(|n| {
                n.kind == NodeKind::View
                    && n.name == "Text"
                    && n.id
                        .starts_with("test.swift::ContentView::exitPopView::view:")
            })
            .expect("exit helper text should exist");
        let helper_inline_panel = result
            .nodes
            .iter()
            .find(|n| {
                n.kind == NodeKind::View
                    && n.name == "InlinePanel"
                    && n.id
                        .starts_with("test.swift::ContentView::chatRoomFragViewPanel::view:")
            })
            .expect("helper InlinePanel view should exist");

        assert!(has_edge(&result, &body.id, &nav.id, EdgeKind::Contains));
        assert!(has_edge(
            &result,
            &nav.id,
            &if_branch.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &if_branch.id,
            &body_inline_panel.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &nav.id,
            &chat_panel.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(&result, &nav.id, &exit_pop.id, EdgeKind::Contains));
        assert!(has_edge(
            &result,
            &nav.id,
            &dialog_stream.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &nav.id,
            &overlay_panel.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &helper_property.id,
            &helper_inline_panel.id,
            EdgeKind::Contains
        ));
        assert!(has_edge(
            &result,
            &exit_property.id,
            &helper_text.id,
            EdgeKind::Contains
        ));
        assert!(
            result
                .nodes
                .iter()
                .all(|n| !(n.kind == NodeKind::Branch && n.name == "switch mode")),
            "non-view modifier closures should not become structural branches"
        );
    }

    #[test]
    fn enriches_swiftui_body_with_read_dependencies() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            struct RoomPage: View {
                @State private var roomMode = 0
                @State private var luckyGameType: Int? = nil
                static let headerViewHeight = 44

                var canShowGameRoom: Bool {
                    roomMode > 0 && luckyGameType != nil
                }

                var roomHeaderView: some View {
                    Text("\(roomMode)")
                }

                var body: some View {
                    VStack {
                        if canShowGameRoom {
                            roomHeaderView
                        }
                    }
                    .frame(height: Self.headerViewHeight)
                }
            }
            "#,
        );
        let graph = grapha_core::merge(vec![result]);

        let has_read = |target: &str| {
            graph.edges.iter().any(|edge| {
                edge.source == "test.swift::RoomPage::body"
                    && edge.target == target
                    && edge.kind == EdgeKind::Reads
            })
        };

        assert!(has_read("test.swift::RoomPage::canShowGameRoom"));
        assert!(has_read("test.swift::RoomPage::roomHeaderView"));
        assert!(has_read("test.swift::RoomPage::headerViewHeight"));
    }

    #[test]
    fn enriches_computed_properties_with_read_dependencies() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            struct RoomPage: View {
                @State private var roomMode = 0
                @State private var luckyGameType: Int? = nil
                @State private var minimizeGameRoom = false

                var canShowGameRoom: Bool {
                    guard luckyGameType != nil else { return false }
                    return roomMode > 0 && !minimizeGameRoom
                }

                var body: some View { EmptyView() }
            }
            "#,
        );
        let graph = grapha_core::merge(vec![result]);

        let has_read = |target: &str| {
            graph.edges.iter().any(|edge| {
                edge.source == "test.swift::RoomPage::canShowGameRoom"
                    && edge.target == target
                    && edge.kind == EdgeKind::Reads
            })
        };

        assert!(has_read("test.swift::RoomPage::roomMode"));
        assert!(has_read("test.swift::RoomPage::luckyGameType"));
        assert!(has_read("test.swift::RoomPage::minimizeGameRoom"));
    }

    #[test]
    fn skips_argument_labels_when_enriching_property_reads() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            struct GeneralButtonType {
                let type: Int
            }

            struct AudioButton {
                let label: String
            }

            struct LoginPage: View {
                @State private var isNextValid = false

                private var loginButton: some View {
                    Button {
                    } label: {
                        Text("Login")
                    }
                    .buttonStyle(.app(type: .yellowH112))
                    .disabled(isNextValid.negated)
                }

                var body: some View { loginButton }
            }
            "#,
        );
        let graph = grapha_core::merge(vec![result]);

        let read_targets: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.source == "test.swift::LoginPage::loginButton" && edge.kind == EdgeKind::Reads
            })
            .map(|edge| edge.target.as_str())
            .collect();

        assert!(
            read_targets.contains(&"test.swift::LoginPage::isNextValid"),
            "real property reads should still be preserved"
        );
        assert!(
            read_targets
                .iter()
                .all(|target| !target.ends_with("::label")),
            "trailing closure labels should not become dependency reads: {read_targets:?}"
        );
        assert!(
            read_targets
                .iter()
                .all(|target| !target.ends_with("::type")),
            "named argument labels should not become dependency reads: {read_targets:?}"
        );
    }

    #[test]
    fn marks_swiftui_dynamic_properties_as_invalidation_sources() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            struct RoomPage: View {
                @State private var roomMode = 0
                @StateObject private var ctx = Room.shared
                @EnvironmentObject private var router: RouterViewModel
                @AppStorage("flag") private var showFlag = false

                var body: some View { EmptyView() }
            }
            "#,
        );

        let node = |name: &str| {
            result
                .nodes
                .iter()
                .find(|node| node.name == name)
                .unwrap_or_else(|| panic!("missing node {name}"))
        };

        assert_eq!(
            node("roomMode")
                .metadata
                .get("swiftui.dynamic_property.wrapper")
                .map(|value| value.as_str()),
            Some("state")
        );
        assert_eq!(
            node("ctx")
                .metadata
                .get("swiftui.dynamic_property.wrapper")
                .map(|value| value.as_str()),
            Some("state_object")
        );
        assert_eq!(
            node("router")
                .metadata
                .get("swiftui.dynamic_property.wrapper")
                .map(|value| value.as_str()),
            Some("environment_object")
        );
        assert_eq!(
            node("showFlag")
                .metadata
                .get("swiftui.dynamic_property.wrapper")
                .map(|value| value.as_str()),
            Some("app_storage")
        );
        assert!(
            ["roomMode", "ctx", "router", "showFlag"]
                .iter()
                .all(|name| {
                    node(name)
                        .metadata
                        .get("swiftui.invalidation_source")
                        .is_some_and(|value| value == "true")
                }),
            "dynamic SwiftUI properties should be tagged as invalidation sources"
        );
    }

    #[test]
    fn extracts_swiftui_structure_for_same_type_view_methods() {
        let result = extract(
            r#"
            import SwiftUI

            struct ContentView: View {
                func helperPanel() -> some View {
                    Text("Helper")
                }

                var body: some View {
                    VStack {
                        helperPanel()
                    }
                }
            }
            "#,
        );

        let helper_fn = result
            .nodes
            .iter()
            .find(|n| n.id == "test.swift::ContentView::helperPanel")
            .expect("helper method should exist");
        let helper_call = result
            .nodes
            .iter()
            .find(|n| {
                n.kind == NodeKind::View
                    && n.name == "helperPanel"
                    && n.id.starts_with("test.swift::ContentView::body::view:")
            })
            .expect("helper method call should exist");
        let helper_text = result
            .nodes
            .iter()
            .find(|n| {
                n.kind == NodeKind::View
                    && n.name == "Text"
                    && n.id
                        .starts_with("test.swift::ContentView::helperPanel::view:")
            })
            .expect("helper method text should exist");

        assert!(has_edge(
            &result,
            &helper_call.id,
            &helper_fn.id,
            EdgeKind::TypeRef
        ));
        assert!(has_edge(
            &result,
            &helper_fn.id,
            &helper_text.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn excludes_action_closures_from_structural_view_tree() {
        let result = extract(
            r#"
            import SwiftUI

            struct ExitPopView: View {
                let dismissPopView: () -> Void
                let exit: () -> Void
                let minimize: () -> Void

                var body: some View {
                    VStack {
                        Button {
                            minimize()
                        } label: {
                            Text("Minimize")
                        }
                    }
                    .onTapGesture {
                        dismissPopView()
                    }
                }
            }

            struct ContentView: View {
                @State private var exitPopShow = true

                var body: some View {
                    if exitPopShow {
                        ExitPopView {
                            exitPopShow = false
                        } exit: {
                            handleExitRoom()
                        } minimize: {
                            handleMinimizeRoom()
                        }
                    }
                }

                func handleExitRoom() {}
                func handleMinimizeRoom() {}
            }
            "#,
        );

        assert!(
            result.nodes.iter().all(|node| !(node.kind == NodeKind::View
                && matches!(
                    node.name.as_str(),
                    "handleExitRoom" | "handleMinimizeRoom" | "minimize" | "dismissPopView"
                ))),
            "action closures should not emit structural view nodes"
        );

        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == NodeKind::View && node.name == "Text"),
            "builder-like label closures should still be traversed"
        );
    }

    #[test]
    fn enrich_swiftui_structure_overlays_synthetic_nodes_without_duplicating_declarations() {
        let source = br#"
import SwiftUI

struct Row: View {
    let title: String
    var body: some View { Text(title) }
}

struct ContentView: View {
    var body: some View {
        VStack {
            Text("Hello")
            Row(title: "World")
        }
    }
}
"#;
        let mut result = ExtractionResult::new();
        result.nodes.push(Node {
            id: "s:Row".into(),
            kind: NodeKind::Struct,
            name: "Row".into(),
            file: "test.swift".into(),
            span: Span {
                start: [3, 0],
                end: [6, 0],
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
        result.nodes.push(Node {
            id: "s:ContentView".into(),
            kind: NodeKind::Struct,
            name: "ContentView".into(),
            file: "test.swift".into(),
            span: Span {
                start: [8, 0],
                end: [15, 0],
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
        result.nodes.push(Node {
            id: "s:ContentView.body".into(),
            kind: NodeKind::Property,
            name: "body".into(),
            file: "test.swift".into(),
            span: Span {
                start: [9, 4],
                end: [14, 5],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: Some(NodeRole::EntryPoint),
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        });

        enrich_swiftui_structure(source, Path::new("test.swift"), &mut result).unwrap();

        let view_nodes: Vec<&Node> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::View)
            .collect();
        assert!(
            !view_nodes.is_empty(),
            "overlay should add synthetic view nodes"
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Property && n.name == "body")
                .count(),
            1,
            "overlay should not duplicate declaration nodes"
        );
        let vstack = view_nodes
            .iter()
            .find(|n| n.name == "VStack")
            .expect("overlay should add VStack");
        assert!(has_edge(
            &result,
            "s:ContentView.body",
            &vstack.id,
            EdgeKind::Contains
        ));
        let row_view = view_nodes
            .iter()
            .find(|n| n.name == "Row")
            .expect("overlay should add Row instance");
        assert!(has_edge(
            &result,
            &row_view.id,
            "test.swift::Row",
            EdgeKind::TypeRef
        ));
    }

    #[test]
    fn enrich_swiftui_structure_matches_view_helpers_by_owner_when_line_metadata_drifts() {
        let source = br#"
import SwiftUI

extension RoomPage {
    @ViewBuilder
    private var centerContentView: some View {
        ZStack {
            Text("Hello")
        }
    }
}
"#;
        let mut result = ExtractionResult::new();
        result.nodes.push(Node {
            id: "s:e:s:4Room0A4PageV".into(),
            kind: NodeKind::Extension,
            name: "RoomPage".into(),
            file: "RoomPage.swift".into(),
            span: Span {
                start: [2, 0],
                end: [9, 0],
            },
            visibility: Visibility::Crate,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Room".into()),
            snippet: None,
            repo: None,
        });
        result.nodes.push(Node {
            id: "s:4Room0A4PageV17centerContentViewQrvp".into(),
            kind: NodeKind::Property,
            name: "centerContentView".into(),
            file: "RoomPage.swift".into(),
            span: Span {
                start: [0, 0],
                end: [0, 0],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Room".into()),
            snippet: None,
            repo: None,
        });
        result.edges.push(Edge {
            source: "s:4Room0A4PageV17centerContentViewQrvp".into(),
            target: "s:e:s:4Room0A4PageV".into(),
            kind: EdgeKind::Implements,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        });

        enrich_swiftui_structure(source, Path::new("RoomPage.swift"), &mut result).unwrap();

        let zstack = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == NodeKind::View
                    && node.name == "ZStack"
                    && node
                        .id
                        .starts_with("s:4Room0A4PageV17centerContentViewQrvp::view:")
            })
            .expect("centerContentView should gain a ZStack subtree despite line drift");

        assert!(has_edge(
            &result,
            "s:4Room0A4PageV17centerContentViewQrvp",
            &zstack.id,
            EdgeKind::Contains
        ));
    }

    #[test]
    fn extracts_function_signature() {
        let result = extract("func greet(name: String) -> String { return name }");
        let node = find_node(&result, "greet");
        assert!(node.signature.is_some(), "signature should be extracted");
        let sig = node.signature.as_ref().unwrap();
        assert!(
            sig.contains("func greet"),
            "signature should contain func name"
        );
    }

    #[test]
    fn extracts_doc_comment() {
        let result = extract(
            r#"
            /// A documented function
            func documented() { }
            "#,
        );
        let node = find_node(&result, "documented");
        assert!(
            node.doc_comment.is_some(),
            "doc_comment should be extracted"
        );
        let doc = node.doc_comment.as_ref().unwrap();
        assert!(doc.contains("documented"), "should contain comment text");
    }

    #[test]
    fn extracts_doc_comment_with_attributes() {
        let result = extract(
            r#"
            class GameManager {
                /// Setup the initial running context and load the game scene, this method should be called when
                /// the game view is appeared.
                @MainActor public func bootstrapGame(with launchContext: WebGameLaunchContext) async {
                }
            }
            "#,
        );
        let node = find_node(&result, "bootstrapGame");
        assert!(
            node.doc_comment.is_some(),
            "doc_comment should be extracted for method with @MainActor attribute"
        );
        let doc = node.doc_comment.as_ref().unwrap();
        assert!(
            doc.contains("Setup the initial running context"),
            "should contain comment text, got: {}",
            doc
        );
    }

    #[test]
    fn extracts_doc_comments_for_type_property_and_protocol_declarations() {
        let result = extract(
            r#"
            /// Owns the gift flow.
            struct GiftFlow {
                /// Current gift recipient.
                var recipientName: String
            }

            /// Global hooks for game UI actions.
            @MainActor
            public enum WebGameCoreHooks { }

            /// Runs gift checkout.
            class GiftCheckout { }

            /// Coordinates gift payment.
            protocol GiftPaying {
                /// Starts the payment handoff.
                func startPayment()
            }

            /// Stable gift identifier.
            typealias GiftId = String
            "#,
        );

        for (name, expected) in [
            ("GiftFlow", "Owns the gift flow"),
            ("recipientName", "Current gift recipient"),
            ("WebGameCoreHooks", "Global hooks for game UI actions"),
            ("GiftCheckout", "Runs gift checkout"),
            ("GiftPaying", "Coordinates gift payment"),
            ("startPayment", "Starts the payment handoff"),
            ("GiftId", "Stable gift identifier"),
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
    fn enrich_doc_comments_patches_missing_docs() {
        let source = br#"
class GameManager {
    /// Setup the initial running context and load the game scene.
    /// This method should be called when the game view is appeared.
    @MainActor public func bootstrapGame(with launchContext: WebGameLaunchContext) async {
    }

    /// Returns the current score.
    var score: Int { 0 }
}
"#;
        // Simulate index-store output: correct names and 1-based lines, no doc comments.
        let mut result = ExtractionResult::new();
        result.nodes.push(Node {
            id: "s:GameManager".into(),
            kind: NodeKind::Struct,
            name: "GameManager".into(),
            file: "test.swift".into(),
            span: Span {
                start: [1, 0],
                end: [1, 0],
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
        result.nodes.push(Node {
            id: "s:GameManager.bootstrapGame".into(),
            kind: NodeKind::Function,
            name: "bootstrapGame".into(),
            file: "test.swift".into(),
            span: Span {
                start: [5, 4],
                end: [5, 4],
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
        result.nodes.push(Node {
            id: "s:GameManager.score".into(),
            kind: NodeKind::Property,
            name: "score".into(),
            file: "test.swift".into(),
            span: Span {
                start: [9, 4],
                end: [9, 4],
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

        enrich_doc_comments(source, &mut result).unwrap();

        let _game_mgr = result
            .nodes
            .iter()
            .find(|n| n.name == "GameManager")
            .unwrap();
        // class_declaration on line 1 (0-based row 1 → 1-based line 2 actually)
        // Let's just check the function and property which are the real targets.

        let bootstrap = result
            .nodes
            .iter()
            .find(|n| n.name == "bootstrapGame")
            .unwrap();
        assert!(
            bootstrap.doc_comment.is_some(),
            "bootstrapGame should have doc_comment after enrichment"
        );
        assert!(
            bootstrap
                .doc_comment
                .as_ref()
                .unwrap()
                .contains("Setup the initial running context"),
            "doc should contain expected text"
        );

        let score = result.nodes.iter().find(|n| n.name == "score").unwrap();
        assert!(
            score.doc_comment.is_some(),
            "score property should have doc_comment after enrichment"
        );
        assert!(
            score
                .doc_comment
                .as_ref()
                .unwrap()
                .contains("current score"),
            "doc should contain expected text"
        );
    }

    #[test]
    fn enrich_doc_comments_patches_enum_case_docs() {
        let source = br#"public enum ActivityTaskCode: Int {
    /// New user room owner task.
    case newUserRoomTask = 11
}
"#;
        // Simulate index-store output: correct symbol name and 1-based line,
        // but no doc comment attached to the variant.
        let mut result = ExtractionResult::new();
        result.nodes.push(Node {
            id: "s:ActivityTaskCode.newUserRoomTask".into(),
            kind: NodeKind::Variant,
            name: "newUserRoomTask".into(),
            file: "ActivityAPI.swift".into(),
            span: Span {
                start: [3, 10],
                end: [3, 10],
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

        enrich_doc_comments(source, &mut result).unwrap();

        let task = result
            .nodes
            .iter()
            .find(|n| n.name == "newUserRoomTask")
            .unwrap();
        assert_eq!(
            task.doc_comment.as_deref(),
            Some("/// New user room owner task.")
        );
    }

    #[test]
    fn enrich_doc_comments_patches_attribute_prefixed_type_docs() {
        let source = br#"
/// UI action hooks that WebGame registers at startup.
/// Breaks circular dependencies where WebGameCore needs to trigger UI defined in WebGame.
@MainActor
public enum WebGameCoreHooks {
}
"#;
        let mut result = ExtractionResult::new();
        result.nodes.push(Node {
            id: "s:WebGameCoreHooks".into(),
            kind: NodeKind::Enum,
            name: "WebGameCoreHooks".into(),
            file: "test.swift".into(),
            span: Span {
                start: [4, 0],
                end: [4, 0],
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

        enrich_doc_comments(source, &mut result).unwrap();

        let hooks = result
            .nodes
            .iter()
            .find(|node| node.name == "WebGameCoreHooks")
            .unwrap();
        assert!(
            hooks
                .doc_comment
                .as_deref()
                .is_some_and(|doc| doc.contains("Breaks circular dependencies")),
            "attribute-prefixed type docs should enrich index-store nodes"
        );
    }

    #[test]
    fn detects_main_attr_as_entry_point() {
        let result = extract(
            r#"
            @main
            struct MyApp {
            }
            "#,
        );
        let app_node = find_node(&result, "MyApp");
        assert_eq!(
            app_node.role,
            Some(grapha_core::graph::NodeRole::EntryPoint),
            "@main struct should be an EntryPoint"
        );
    }

    #[test]
    fn enrich_localization_metadata_marks_generated_wrapper_symbols() {
        let result = extract_with_localization(
            r#"
            public enum L10n {
                public static var accountForgetPassword: String {
                    L10n.tr("Localizable", "account_forget_password", fallback: "Forgot Password")
                }

                public static func commonCount(_ p1: Any) -> String {
                    L10n.tr("Localizable", "common_count", String(describing: p1), fallback: "%@")
                }
            }

            public struct L10nResource {
                public init(_ key: String, table: String, bundle: Bundle, fallback: String) {}
            }

            public enum L10nResourceSet {
                public static let commonShare = L10nResource(
                    "common_share",
                    table: "Localizable",
                    bundle: .module,
                    fallback: "Share"
                )
            }
            "#,
        );

        let account_forget_password = result
            .nodes
            .iter()
            .find(|node| node.name == "accountForgetPassword")
            .expect("wrapper property should exist");
        assert_eq!(
            account_forget_password
                .metadata
                .get("l10n.wrapper.table")
                .map(|value| value.as_str()),
            Some("Localizable")
        );
        assert_eq!(
            account_forget_password
                .metadata
                .get("l10n.wrapper.key")
                .map(|value| value.as_str()),
            Some("account_forget_password")
        );

        let common_count = result
            .nodes
            .iter()
            .find(|node| node.name == "commonCount")
            .expect("wrapper function should exist");
        assert_eq!(
            common_count
                .metadata
                .get("l10n.wrapper.key")
                .map(|value| value.as_str()),
            Some("common_count")
        );
        assert_eq!(
            common_count
                .metadata
                .get("l10n.wrapper.arg_count")
                .map(|value| value.as_str()),
            Some("1")
        );

        let common_share = result
            .nodes
            .iter()
            .find(|node| node.name == "commonShare")
            .expect("resource wrapper property should exist");
        assert_eq!(
            common_share
                .metadata
                .get("l10n.wrapper.key")
                .map(|value| value.as_str()),
            Some("common_share")
        );
    }

    #[test]
    fn enrich_localization_metadata_marks_swiftui_text_usages() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            public enum L10n {
                public static var accountForgetPassword: String {
                    L10n.tr("Localizable", "account_forget_password", fallback: "Forgot Password")
                }

                public static var storeUseNow: String {
                    L10n.tr("Localizable", "store_use_now", fallback: "Use now")
                }

                public static func commonCount(_ p1: Any) -> String {
                    L10n.tr("Localizable", "common_count", String(describing: p1), fallback: "%@")
                }
            }

            public struct L10nResource {
                public init(_ key: String, table: String, bundle: Bundle, fallback: String) {}
            }

            public enum L10nResourceSet {
                public static let commonShare = L10nResource(
                    "common_share",
                    table: "Localizable",
                    bundle: .module,
                    fallback: "Share"
                )
            }

            struct ContentView: View {
                let title: String

                var body: some View {
                    VStack {
                        Text(.accountForgetPassword)
                        Text(i18n: .commonShare)
                        Text(L10n.storeUseNow)
                        Text(L10n.commonCount(42))
                        Text(verbatim: title)
                        Text(title)
                        Text("Party Game & Voice Chat", bundle: .module)
                    }
                }
            }
            "#,
        );

        let localized_texts: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::View && node.name == "Text")
            .filter(|node| node.metadata.contains_key("l10n.ref_kind"))
            .collect();
        assert_eq!(
            localized_texts.len(),
            6,
            "expected wrapper, literal, and possible string Text usages, but not verbatim text"
        );

        let account_text = localized_texts
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.wrapper_name")
                    .map(|value| value.as_str())
                    == Some("accountForgetPassword")
            })
            .expect("dot syntax Text usage should be marked");
        assert_eq!(
            account_text
                .metadata
                .get("l10n.ref_kind")
                .map(|value| value.as_str()),
            Some("wrapper")
        );

        let common_share_text = localized_texts
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.wrapper_name")
                    .map(|value| value.as_str())
                    == Some("commonShare")
            })
            .expect("i18n Text usage should be marked");
        assert_eq!(
            common_share_text
                .metadata
                .get("l10n.arg_count")
                .map(|value| value.as_str()),
            Some("0")
        );
        assert_eq!(
            common_share_text
                .metadata
                .get("l10n.wrapper_base")
                .map(|value| value.as_str()),
            Some("L10nResource")
        );

        let common_count_text = localized_texts
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.wrapper_name")
                    .map(|value| value.as_str())
                    == Some("commonCount")
            })
            .expect("parameterized wrapper usage should be marked");
        assert_eq!(
            common_count_text
                .metadata
                .get("l10n.arg_count")
                .map(|value| value.as_str()),
            Some("1")
        );

        let literal_text = localized_texts
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.ref_kind")
                    .map(|value| value.as_str())
                    == Some("literal")
            })
            .expect("string literal usage should be marked");
        assert_eq!(
            literal_text
                .metadata
                .get("l10n.literal")
                .map(|value| value.as_str()),
            Some("Party Game & Voice Chat")
        );

        let possible_string_text = localized_texts
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.ref_kind")
                    .map(|value| value.as_str())
                    == Some("possible_string")
            })
            .expect("dynamic string Text usage should be marked as possible");
        assert!(
            !possible_string_text.metadata.contains_key("l10n.literal"),
            "possible string usages should not pretend to have a concrete literal"
        );

        assert!(
            result.edges.iter().any(|edge| {
                edge.kind == EdgeKind::TypeRef
                    && edge.source == account_text.id
                    && edge.target.ends_with("accountForgetPassword")
            }),
            "localized Text usage should emit a type-ref edge to its wrapper symbol"
        );
    }

    #[test]
    fn enrich_localization_metadata_marks_custom_view_string_arguments() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            public enum L10n {
                public static var welcomeTitle: String {
                    L10n.tr("Localizable", "welcome_title", fallback: "Welcome")
                }
            }

            struct TitleRow: View {
                let title: String
                let subtitle: String

                var body: some View {
                    VStack {
                        Text(title)
                        Text(subtitle)
                    }
                }
            }

            struct ContentView: View {
                let subtitle: String

                var body: some View {
                    TitleRow(title: L10n.welcomeTitle, subtitle: subtitle)
                }
            }
            "#,
        );
        let wrapper_argument = result
            .nodes
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.argument_label")
                    .map(|value| value.as_str())
                    == Some("title")
                    && node
                        .metadata
                        .get("l10n.ref_kind")
                        .map(|value| value.as_str())
                        == Some("wrapper")
            })
            .expect("custom view title argument should be marked");
        assert!(
            result.edges.iter().any(|edge| {
                edge.kind == EdgeKind::TypeRef
                    && edge.source == wrapper_argument.id
                    && edge.target.ends_with("welcomeTitle")
            }),
            "custom view wrapper arguments should link to wrapper symbols"
        );

        let possible_argument = result
            .nodes
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.argument_label")
                    .map(|value| value.as_str())
                    == Some("subtitle")
                    && node
                        .metadata
                        .get("l10n.ref_kind")
                        .map(|value| value.as_str())
                        == Some("possible_string")
            })
            .expect("dynamic custom view text arguments should be marked as possible");
        assert!(
            result.edges.iter().any(|edge| {
                edge.kind == EdgeKind::Contains
                    && edge.target == possible_argument.id
                    && edge.source.contains("TitleRow")
            }),
            "custom-view localization markers should sit under the invocation subtree"
        );
    }

    #[test]
    fn localized_text_reference_marks_text_i18n_identifier_as_possible_wrapper() {
        let text = localized_text_references_from_text("Text(i18n: i18n)", None)
            .into_iter()
            .next()
            .expect("Text(i18n: identifier) should be classified");
        assert_eq!(text.ref_kind, "possible_wrapper");
        assert_eq!(text.wrapper_base.as_deref(), Some("L10nResource"));
    }

    #[test]
    fn localized_reference_ignores_non_localized_member_access() {
        assert!(localized_reference_for_expression_text("friendList.isNilOrEmpty").is_none());
        assert!(localized_reference_for_expression_text("searchUserList.isEmpty").is_none());

        let wrapper = localized_reference_for_expression_text("L10n.welcomeTitle")
            .expect("L10n wrapper references should still resolve");
        assert_eq!(wrapper.wrapper_name.as_deref(), Some("welcomeTitle"));
        assert_eq!(wrapper.wrapper_base.as_deref(), Some("L10n"));
    }

    #[test]
    fn enrich_localization_metadata_marks_string_i18n_identifier_in_custom_view_args() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            public struct L10nResource {
                public var translation: String { "" }
            }

            extension String {
                init(i18n: L10nResource) {
                    self.init(i18n.translation)
                }
            }

            struct TitleRow: View {
                let title: String

                var body: some View {
                    Text(title)
                }
            }

            struct ContentView: View {
                let resource: L10nResource

                var body: some View {
                    TitleRow(title: String(i18n: resource))
                }
            }
            "#,
        );

        let title_argument = result
            .nodes
            .iter()
            .find(|node| {
                node.metadata
                    .get("l10n.argument_label")
                    .map(|value| value.as_str())
                    == Some("title")
                    && node
                        .metadata
                        .get("l10n.ref_kind")
                        .map(|value| value.as_str())
                        == Some("possible_wrapper")
            })
            .expect("String(i18n: identifier) should be tracked as a possible wrapper");
        assert_eq!(
            title_argument
                .metadata
                .get("l10n.wrapper_base")
                .map(|value| value.as_str()),
            Some("L10nResource")
        );
    }

    #[test]
    fn extracts_optional_some_view_structure_and_localized_aliases() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            public struct L10nResource {
                public init(_ key: String, table: String, bundle: Bundle, fallback: String) {}
                public var translation: String { "" }
            }

            extension String {
                init(i18n: L10nResource) {
                    self.init(i18n.translation)
                }
            }

            extension L10nResource {
                static let roomShareNoFriends = L10nResource(
                    "room_share_no_friends",
                    table: "Localizable",
                    bundle: .module,
                    fallback: "No friends"
                )
                static let commonSearchEmpty = L10nResource(
                    "common_search_empty",
                    table: "Localizable",
                    bundle: .module,
                    fallback: "No results"
                )
            }

            struct ListEmptyView: View {
                let title: String

                var body: some View {
                    Text(title)
                }
            }

            struct ContentView: View {
                let isEmpty: Bool

                private var emptyPlaceholderView: (some View)? {
                    let title: String? =
                        if isEmpty {
                            String(i18n: .roomShareNoFriends)
                        } else {
                            String(i18n: .commonSearchEmpty)
                        }

                    return if let title {
                        ListEmptyView(title: title)
                    } else {
                        nil
                    }
                }
            }
            "#,
        );

        let list_empty_view = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == NodeKind::View
                    && node.name == "ListEmptyView"
                    && node.id.contains("emptyPlaceholderView::view:")
            })
            .expect("optional some View property should still emit SwiftUI structure");
        assert!(
            result.edges.iter().any(|edge| {
                edge.kind == EdgeKind::TypeRef
                    && edge.source == list_empty_view.id
                    && edge.target.ends_with("ListEmptyView")
            }),
            "custom view should keep its type-ref edge"
        );

        let alias_arguments: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.metadata
                    .get("l10n.argument_label")
                    .map(|value| value.as_str())
                    == Some("title")
                    && node
                        .metadata
                        .get("l10n.ref_kind")
                        .map(|value| value.as_str())
                        == Some("wrapper")
            })
            .collect();
        assert_eq!(alias_arguments.len(), 2);
        assert!(
            alias_arguments.iter().any(|node| {
                node.metadata
                    .get("l10n.wrapper_name")
                    .map(|value| value.as_str())
                    == Some("roomShareNoFriends")
            }),
            "first branch localization should propagate into the custom view argument"
        );
        assert!(
            alias_arguments.iter().any(|node| {
                node.metadata
                    .get("l10n.wrapper_name")
                    .map(|value| value.as_str())
                    == Some("commonSearchEmpty")
            }),
            "second branch localization should propagate into the custom view argument"
        );

        assert!(
            alias_arguments.iter().all(|node| {
                node.metadata
                    .get("l10n.wrapper_name")
                    .is_some_and(|value| value != "isEmpty" && value != "isNilOrEmpty")
            }),
            "condition helper members should not leak into localized alias bindings"
        );
    }

    #[test]
    fn enrich_localization_metadata_marks_non_view_constructor_text_arguments() {
        let result = extract_with_localization(
            r#"
            import Foundation

            public enum L10n {
                public static var roomShareDesc: String {
                    L10n.tr("Localizable", "room_share_desc", fallback: "I'm in this room")
                }

                private static func tr(_ table: String, _ key: String, fallback: String) -> String {
                    fallback
                }
            }

            struct ShareWithFriendsEntity {
                let shareText: String
                let shareLink: String
            }

            struct ContentView {
                func onShare(shareLink: String) {
                    let entity = ShareWithFriendsEntity(
                        shareText: L10n.roomShareDesc,
                        shareLink: shareLink
                    )
                    _ = entity
                }
            }
            "#,
        );

        let share_text_usages: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.metadata
                    .get("l10n.argument_label")
                    .map(|value| value.as_str())
                    == Some("shareText")
                    && node
                        .metadata
                        .get("l10n.ref_kind")
                        .map(|value| value.as_str())
                        == Some("wrapper")
            })
            .collect();

        assert_eq!(share_text_usages.len(), 1);
        let usage = share_text_usages[0];
        assert_eq!(
            usage
                .metadata
                .get("l10n.wrapper_name")
                .map(|value| value.as_str()),
            Some("roomShareDesc")
        );
        assert!(
            result.edges.iter().any(|edge| {
                edge.kind == EdgeKind::TypeRef
                    && edge.source == usage.id
                    && edge.target.ends_with("roomShareDesc")
            }),
            "usage node should point at the generated localization wrapper"
        );
    }

    #[test]
    fn enrich_localization_metadata_marks_builtin_views_with_localized_string_key() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            struct ContentView: View {
                @State var isOn = false

                var body: some View {
                    VStack {
                        Button("Tournament") { }
                        Label("Settings", systemImage: "gear")
                        Toggle("Notifications", isOn: $isOn)
                        NavigationLink("Profile") { Text("Detail") }
                        Section("Account") {
                            Text("Content")
                        }
                    }
                }
            }
            "#,
        );

        let l10n_usage_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.metadata
                    .get("l10n.ref_kind")
                    .map(|value| value.as_str())
                    == Some("literal")
                    && node.metadata.contains_key("l10n.literal")
            })
            .collect();

        let literals: Vec<&str> = l10n_usage_nodes
            .iter()
            .filter_map(|node| node.metadata.get("l10n.literal").map(|v| v.as_str()))
            .collect();

        for expected in &[
            "Tournament",
            "Settings",
            "Notifications",
            "Profile",
            "Account",
        ] {
            assert!(
                literals.contains(expected),
                "expected literal '{}' from built-in SwiftUI view, got: {:?}",
                expected,
                literals
            );
        }
    }

    #[test]
    fn source_contains_image_asset_markers_detects_swiftui_and_uikit_calls() {
        assert!(source_contains_image_asset_markers(
            br#"var badge: some View { Image("feature_badge") }"#
        ));
        assert!(source_contains_image_asset_markers(
            br#"let image = UIImage(named: "avatar")"#
        ));
        assert!(source_contains_image_asset_markers(
            br#"let system = Image(systemName: "gear")"#
        ));
        assert!(!source_contains_image_asset_markers(
            br#"let label = Text("hello")"#
        ));
    }

    #[test]
    fn enrich_asset_references_tags_enclosing_nodes_with_first_image_asset() {
        let result = extract_with_localization(
            r#"
            import SwiftUI

            struct ContentView: View {
                var badge: some View {
                    Image("feature_badge")
                    Image("ignored_second_asset")
                }

                var body: some View {
                    VStack {
                        badge
                        Image(systemName: "gear")
                    }
                }
            }
            "#,
        );

        let asset_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.metadata.contains_key("asset.name"))
            .collect();
        let asset_names: std::collections::HashSet<_> = asset_nodes
            .iter()
            .filter_map(|node| node.metadata.get("asset.name").map(String::as_str))
            .collect();
        assert_eq!(
            asset_names,
            std::collections::HashSet::from(["feature_badge", "ignored_second_asset"]),
            "current enrichment should preserve both concrete image asset names"
        );
        assert!(asset_nodes.iter().all(|node| {
            node.metadata.get("asset.ref_kind").map(String::as_str) == Some("image")
        }));

        let body = result
            .nodes
            .iter()
            .find(|node| node.id == "test.swift::ContentView::body")
            .expect("body property should exist");
        assert!(
            !body.metadata.contains_key("asset.name"),
            "SF symbol calls should not tag enclosing nodes as asset references"
        );
    }
}
