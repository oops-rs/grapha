use grapha_core::graph::{EdgeKind, TerminalKind};

pub(crate) fn is_dataflow_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::Reads
            | EdgeKind::Writes
            | EdgeKind::Publishes
            | EdgeKind::Subscribes
    )
}

pub(crate) fn terminal_kind_to_string(kind: &TerminalKind) -> String {
    match kind {
        TerminalKind::Network => "network".to_string(),
        TerminalKind::Persistence => "persistence".to_string(),
        TerminalKind::Cache => "cache".to_string(),
        TerminalKind::Event => "event".to_string(),
        TerminalKind::Keychain => "keychain".to_string(),
        TerminalKind::Search => "search".to_string(),
    }
}
