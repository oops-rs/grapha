use std::path::Path;

use grapha_core::LanguageExtractor;
use grapha_core::graph::EdgeKind;
use grapha_swift::SwiftExtractor;

#[test]
fn fallback_preserves_declared_protocol_type_and_receiver_call_target() {
    let source = br#"
        protocol Session {
            func leave()
        }

        final class Controller {
            var session: any Session

            func close() {
                session.leave()
                session.events.observe()
                bare()
            }

            func bare() {}
        }
    "#;
    let path = Path::new("receiver_calls.swift");
    let result = SwiftExtractor
        .extract(source, path)
        .expect("Swift tree-sitter fallback should extract the fixture");

    let session = result
        .nodes
        .iter()
        .find(|node| node.name == "session")
        .expect("missing session property");
    assert_eq!(
        session
            .metadata
            .get("grapha.declared_type")
            .map(String::as_str),
        Some("Session"),
        "the existential marker must not become part of the receiver type"
    );

    let close = result
        .nodes
        .iter()
        .find(|node| node.name == "close")
        .expect("missing close function");
    let receiver_call = result
        .edges
        .iter()
        .find(|edge| {
            edge.source == close.id
                && edge.kind == EdgeKind::Calls
                && edge.target == "session.leave"
        })
        .expect("missing raw receiver-qualified call target");
    assert_eq!(receiver_call.confidence, 0.8);
    assert_eq!(receiver_call.provenance.len(), 1);
    assert_eq!(receiver_call.provenance[0].file, path);
    assert_eq!(receiver_call.provenance[0].symbol_id, close.id);

    assert!(
        result.edges.iter().any(|edge| {
            edge.source == close.id
                && edge.kind == EdgeKind::Calls
                && edge.target == "session.events.observe"
        }),
        "nested receiver chains must retain each navigation component"
    );

    assert!(
        result.edges.iter().any(|edge| {
            edge.source == close.id
                && edge.kind == EdgeKind::Calls
                && edge.target == "receiver_calls.swift::bare"
        }),
        "bare-call target generation must remain file-scoped"
    );
}
