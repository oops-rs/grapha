use crate::classify::{Classification, Classifier, ClassifyContext};
use grapha_core::graph::{FlowDirection, TerminalKind};
use grapha_core::semantic::TerminalEffect;

#[cfg_attr(not(test), allow(dead_code))]
pub struct RustClassifier;

impl RustClassifier {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new() -> Self {
        Self
    }
}

impl Default for RustClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Classifier for RustClassifier {
    fn classify(&self, call_target: &str, _context: &ClassifyContext) -> Option<Classification> {
        classify_rust(call_target)
    }
}

pub(crate) fn terminal_effect_for_target(target: &str) -> Option<TerminalEffect> {
    classify_rust(target).map(|classification| TerminalEffect {
        terminal_kind: classification.terminal_kind,
        direction: classification.direction,
        operation: classification.operation,
    })
}

fn classify_rust(target: &str) -> Option<Classification> {
    // std::fs — Persistence
    if matches_any(
        target,
        &[
            "std::fs::",
            "fs::read",
            "fs::write",
            "fs::remove",
            "fs::create_dir",
            "fs::copy",
            "fs::rename",
        ],
    ) {
        let direction = if contains_any(
            target,
            &["read", "metadata", "exists", "read_dir", "canonicalize"],
        ) {
            FlowDirection::Read
        } else if contains_any(target, &["write", "remove", "create_dir", "copy", "rename"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Persistence,
            direction,
            operation: "fs".to_string(),
        });
    }

    // rusqlite — Persistence
    if matches_any(
        target,
        &[
            "Connection::",
            "connection.",
            "Statement::",
            "statement.",
            "rusqlite",
        ],
    ) {
        let direction = if contains_any(target, &["query", "prepare", "select", "get"]) {
            FlowDirection::Read
        } else if contains_any(target, &["execute", "insert", "update", "delete"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Persistence,
            direction,
            operation: "SQLite".to_string(),
        });
    }

    // tantivy — Search
    if matches_any(
        target,
        &["IndexWriter", "IndexReader", "Searcher", "tantivy"],
    ) {
        let direction = if contains_any(target, &["Searcher", "search", "reader", "Reader"]) {
            FlowDirection::Read
        } else if contains_any(
            target,
            &["Writer", "writer", "add_document", "commit", "delete"],
        ) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Search,
            direction,
            operation: "tantivy".to_string(),
        });
    }

    // tokio channels — Event
    if matches_any(
        target,
        &[
            "mpsc::channel",
            "mpsc::unbounded",
            "broadcast::channel",
            "watch::channel",
            "oneshot::channel",
            "tx.send",
            "rx.recv",
            "Sender",
            "Receiver",
        ],
    ) {
        let direction = if contains_any(target, &["send", "Sender", "tx"]) {
            FlowDirection::Write
        } else if contains_any(target, &["recv", "Receiver", "rx"]) {
            FlowDirection::Read
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Event,
            direction,
            operation: "channel".to_string(),
        });
    }

    // reqwest — Network
    if matches_any(
        target,
        &[
            "reqwest",
            "Client::new",
            "client.get",
            "client.post",
            "client.put",
            "client.delete",
        ],
    ) {
        let direction = if contains_any(target, &["get"]) {
            FlowDirection::Read
        } else if contains_any(target, &["post", "put", "delete", "patch"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Network,
            direction,
            operation: "HTTP".to_string(),
        });
    }

    None
}

fn matches_any(target: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| target.contains(p))
}

fn contains_any(target: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| target.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> ClassifyContext {
        ClassifyContext {
            source_node: "test::caller".to_string(),
            file: PathBuf::from("test.rs"),
            arguments: vec![],
        }
    }

    #[test]
    fn classifies_fs_read_as_persistence_read() {
        let c = RustClassifier::new();
        let result = c.classify("std::fs::read_to_string", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "fs");
    }

    #[test]
    fn classifies_fs_write_as_persistence_write() {
        let c = RustClassifier::new();
        let result = c.classify("std::fs::write", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "fs");
    }

    #[test]
    fn classifies_rusqlite_query_as_persistence_read() {
        let c = RustClassifier::new();
        let result = c.classify("Connection::query", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "SQLite");
    }

    #[test]
    fn classifies_rusqlite_execute_as_persistence_write() {
        let c = RustClassifier::new();
        let result = c.classify("connection.execute", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "SQLite");
    }

    #[test]
    fn classifies_tantivy_writer_as_search_write() {
        let c = RustClassifier::new();
        let result = c.classify("IndexWriter::add_document", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Search);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "tantivy");
    }

    #[test]
    fn classifies_tantivy_searcher_as_search_read() {
        let c = RustClassifier::new();
        let result = c.classify("Searcher.search", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Search);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "tantivy");
    }

    #[test]
    fn classifies_tokio_tx_send_as_event_write() {
        let c = RustClassifier::new();
        let result = c.classify("tx.send", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Event);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "channel");
    }

    #[test]
    fn classifies_tokio_rx_recv_as_event_read() {
        let c = RustClassifier::new();
        let result = c.classify("rx.recv", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Event);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "channel");
    }

    #[test]
    fn classifies_reqwest_get_as_network_read() {
        let c = RustClassifier::new();
        let result = c.classify("reqwest::get", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Network);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "HTTP");
    }

    #[test]
    fn classifies_reqwest_post_as_network_write() {
        let c = RustClassifier::new();
        let result = c.classify("client.post", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Network);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "HTTP");
    }

    #[test]
    fn returns_none_for_unknown() {
        let c = RustClassifier::new();
        let result = c.classify("my_custom_function", &ctx());
        assert!(result.is_none());
    }
}
