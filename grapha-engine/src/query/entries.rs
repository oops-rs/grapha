use serde::Serialize;

use grapha_core::graph::{Graph, Node, NodeKind, NodeRole};

use super::{SymbolRef, file_matches_path_or_suffix};

#[derive(Debug, Clone, Default)]
pub struct EntriesQueryOptions {
    pub module: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct EntriesResult {
    pub entries: Vec<SymbolRef>,
    pub shown: usize,
    pub total: usize,
}

/// Conventional `#[cfg(test)] mod`/dir names whose presence in a symbol id marks
/// a function as a test entry point. Only the plural `tests` is used: a singular
/// `test` module or directory is too ambiguous (real code legitimately has a
/// `test` subcommand module or a `crates/test` member) to treat as a test.
const TEST_MODULE_SEGMENTS: &[&str] = &["tests"];

/// Rank buckets for *real* (non-test) entry points so the genuinely useful
/// entry points (process mains, CLI/axum dispatch fns, public API) surface
/// before incidental crate-root publics.
mod entry_rank {
    pub const MAIN: usize = 0;
    pub const RUN_DISPATCH: usize = 1;
    pub const PUBLIC_API: usize = 2;
    pub const OTHER: usize = 3;
}

/// CLI / axum / service dispatch function names that act as real entry points.
const DISPATCH_FN_NAMES: &[&str] = &["run", "dispatch", "serve", "handle", "execute", "start"];

fn path_has_segment(file: &std::path::Path, segments: &[&str]) -> bool {
    file.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|segment| segments.contains(&segment))
    })
}

fn file_is_test_path(file: &std::path::Path) -> bool {
    if path_has_segment(file, TEST_MODULE_SEGMENTS) {
        return true;
    }
    let normalized = file.to_string_lossy().replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or(&normalized);
    // Note: only the plural `tests.rs` is the conventional inline-test module
    // file; a singular `test.rs` is too ambiguous to treat as a test file.
    base == "tests.rs" || base.ends_with("_test.rs") || base.ends_with("_tests.rs")
}

/// Detect a test function entry point without needing the original `#[test]`
/// attribute (which is not preserved post-merge): a function declared in a
/// `tests/` dir or `*_test.rs` file, or nested under a `tests` module segment in
/// its symbol id (the `#[cfg(test)] mod tests` convention).
pub(crate) fn is_test_entry(node: &Node) -> bool {
    if node.kind != NodeKind::Function && node.kind != NodeKind::Method {
        return false;
    }
    if file_is_test_path(&node.file) {
        return true;
    }
    // Skip the leading file-path component of the id before scanning module
    // segments so a file literally named `tests.rs` doesn't double-trip here.
    node.id
        .split("::")
        .skip(1)
        .any(|segment| TEST_MODULE_SEGMENTS.contains(&segment))
}

fn entry_priority(node: &Node) -> usize {
    if node.name == "main" {
        return entry_rank::MAIN;
    }
    if DISPATCH_FN_NAMES.contains(&node.name.as_str()) {
        return entry_rank::RUN_DISPATCH;
    }
    if node.visibility == grapha_core::graph::Visibility::Public {
        return entry_rank::PUBLIC_API;
    }
    entry_rank::OTHER
}

fn sort_entries_ranked(entries: &mut [(usize, SymbolRef)]) {
    entries.sort_by(|(left_rank, left), (right_rank, right)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| {
                left.module
                    .as_deref()
                    .unwrap_or("")
                    .cmp(right.module.as_deref().unwrap_or(""))
            })
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn collect_entries(
    graph: &Graph,
    options: &EntriesQueryOptions,
    include_tests: bool,
) -> EntriesResult {
    let mut ranked: Vec<(usize, SymbolRef)> = graph
        .nodes
        .iter()
        .filter(|node| node.role == Some(NodeRole::EntryPoint))
        .filter(|node| include_tests || !is_test_entry(node))
        .filter(|node| {
            options
                .module
                .as_deref()
                .is_none_or(|module| node.module.as_deref() == Some(module))
        })
        .filter(|node| {
            options
                .file
                .as_deref()
                .is_none_or(|file_query| file_matches_path_or_suffix(&node.file, file_query))
        })
        .map(|node| (entry_priority(node), SymbolRef::from_node(node)))
        .collect();

    sort_entries_ranked(&mut ranked);
    let mut entries: Vec<SymbolRef> = ranked.into_iter().map(|(_, entry)| entry).collect();

    let total = entries.len();
    let shown = options.limit.map(|limit| limit.min(total)).unwrap_or(total);
    entries.truncate(shown);

    EntriesResult {
        entries,
        shown,
        total,
    }
}

/// List auto-detected entry points, excluding test functions and ranking the
/// real entry points (process mains, CLI/axum dispatch fns, public API) first.
/// Use [`query_entries_with_options_including_tests`] to also surface tests.
pub fn query_entries_with_options(graph: &Graph, options: &EntriesQueryOptions) -> EntriesResult {
    collect_entries(graph, options, false)
}

/// Same as [`query_entries_with_options`] but keeps test-function entry points
/// in the result (still ranked after real entries) for callers that explicitly
/// want them.
pub fn query_entries_with_options_including_tests(
    graph: &Graph,
    options: &EntriesQueryOptions,
) -> EntriesResult {
    collect_entries(graph, options, true)
}

pub fn query_entries(graph: &Graph) -> EntriesResult {
    query_entries_with_options(graph, &EntriesQueryOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, role: Option<NodeRole>) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    fn entry_node(id: &str, name: &str, file: &str, module: Option<&str>) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: name.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: Some(NodeRole::EntryPoint),
            signature: None,
            doc_comment: None,
            module: module.map(str::to_string),
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn lists_entry_points() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry1", Some(NodeRole::EntryPoint)),
                make_node("entry2", Some(NodeRole::EntryPoint)),
                make_node("internal", Some(NodeRole::Internal)),
            ],
            edges: vec![],
        };

        let result = query_entries(&graph);
        assert_eq!(result.total, 2);
        let names: Vec<&str> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"entry1"));
        assert!(names.contains(&"entry2"));
        assert!(!names.contains(&"internal"));
    }

    #[test]
    fn returns_empty_when_no_entries() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("a", None),
                make_node("b", Some(NodeRole::Internal)),
            ],
            edges: vec![],
        };

        let result = query_entries(&graph);
        assert_eq!(result.total, 0);
        assert!(result.entries.is_empty());
    }

    #[test]
    fn filters_entries_by_module_and_file_and_limit() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                entry_node(
                    "room_body",
                    "body",
                    "Modules/Room/Sources/Room/View/RoomPage.swift",
                    Some("Room"),
                ),
                entry_node(
                    "room_share",
                    "onShare",
                    "Modules/Room/Sources/Room/View/RoomPage.swift",
                    Some("Room"),
                ),
                entry_node(
                    "chat_body",
                    "body",
                    "Modules/Chat/Sources/Chat/View/ChatPage.swift",
                    Some("Chat"),
                ),
            ],
            edges: vec![],
        };

        let result = query_entries_with_options(
            &graph,
            &EntriesQueryOptions {
                module: Some("Room".to_string()),
                file: Some("RoomPage.swift".to_string()),
                limit: Some(1),
            },
        );
        let actual: Vec<(&str, &str, &str, Option<&str>)> = result
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.id.as_str(),
                    entry.name.as_str(),
                    entry.file.rsplit('/').next().unwrap_or(entry.file.as_str()),
                    entry.module.as_deref(),
                )
            })
            .collect();
        let expected = vec![("room_body", "body", "RoomPage.swift", Some("Room"))];

        assert_eq!(actual, expected);
        assert_eq!(result.total, 2);
        assert_eq!(result.shown, 1);
    }

    #[test]
    fn file_filter_does_not_match_partial_fragments() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![entry_node(
                "room_body",
                "body",
                "Modules/Room/Sources/Room/View/RoomPage.swift",
                Some("Room"),
            )],
            edges: vec![],
        };

        let result = query_entries_with_options(
            &graph,
            &EntriesQueryOptions {
                file: Some("Page".to_string()),
                ..EntriesQueryOptions::default()
            },
        );

        assert_eq!(result.total, 0);
        assert_eq!(result.shown, 0);
        assert!(result.entries.is_empty());
    }

    fn fn_node(id: &str, name: &str, file: &str, visibility: Visibility) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: name.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility,
            metadata: HashMap::new(),
            role: Some(NodeRole::EntryPoint),
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn excludes_test_functions_by_default_and_ranks_real_entries_first() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                // #[test] fn in a tests/ directory
                fn_node(
                    "apps/x/tests/proxy.rs::ask_works",
                    "ask_works",
                    "apps/x/tests/proxy.rs",
                    Visibility::Public,
                ),
                // #[test] fn under a #[cfg(test)] mod tests in a source file
                fn_node(
                    "crates/x/src/config.rs::tests::a_key_builds",
                    "a_key_builds",
                    "crates/x/src/config.rs",
                    Visibility::Public,
                ),
                // entry in a *_test.rs file
                fn_node(
                    "crates/x/src/foo_test.rs::checks",
                    "checks",
                    "crates/x/src/foo_test.rs",
                    Visibility::Public,
                ),
                // a real process main
                fn_node(
                    "apps/x/src/main.rs::main",
                    "main",
                    "apps/x/src/main.rs",
                    Visibility::Public,
                ),
                // a CLI dispatch fn
                fn_node(
                    "apps/x/src/cli.rs::run",
                    "run",
                    "apps/x/src/cli.rs",
                    Visibility::Public,
                ),
                // public API surface
                fn_node(
                    "crates/x/src/lib.rs::extract",
                    "extract",
                    "crates/x/src/lib.rs",
                    Visibility::Public,
                ),
            ],
            edges: vec![],
        };

        let result = query_entries(&graph);
        let names: Vec<&str> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            result.total, 3,
            "the 3 test functions must be excluded by default"
        );
        assert_eq!(
            names,
            vec!["main", "run", "extract"],
            "real entries should be ranked main > dispatch > public api"
        );
    }

    #[test]
    fn including_tests_keeps_them_after_real_entries() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                fn_node(
                    "crates/x/src/config.rs::tests::a_key_builds",
                    "a_key_builds",
                    "crates/x/src/config.rs",
                    Visibility::Public,
                ),
                fn_node(
                    "apps/x/src/main.rs::main",
                    "main",
                    "apps/x/src/main.rs",
                    Visibility::Public,
                ),
            ],
            edges: vec![],
        };

        let result =
            query_entries_with_options_including_tests(&graph, &EntriesQueryOptions::default());
        assert_eq!(result.total, 2);
        let names: Vec<&str> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["main", "a_key_builds"],
            "tests are kept but ranked after real entries"
        );
    }

    #[test]
    fn is_test_entry_detects_cfg_test_module_segment() {
        let node = fn_node(
            "crates/x/src/config.rs::tests::a_key_builds",
            "a_key_builds",
            "crates/x/src/config.rs",
            Visibility::Public,
        );
        assert!(is_test_entry(&node));

        let non_test = fn_node(
            "crates/x/src/config.rs::load",
            "load",
            "crates/x/src/config.rs",
            Visibility::Public,
        );
        assert!(!is_test_entry(&non_test));
    }
}
