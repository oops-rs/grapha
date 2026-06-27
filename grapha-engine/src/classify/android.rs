use grapha_core::TerminalEffect;
use grapha_core::graph::{FlowDirection, TerminalKind};

pub fn terminal_effect_for_target(target: &str) -> Option<TerminalEffect> {
    let target = target.to_ascii_lowercase();

    if contains_any(
        &target,
        &[
            "retrofit",
            "okhttp",
            "okhttpclient",
            "httpurlconnection",
            "urlconnection",
            "firebasefirestore",
            "firebaseremoteconfig",
            ".newcall",
            ".enqueue",
        ],
    ) {
        return Some(TerminalEffect {
            terminal_kind: TerminalKind::Network,
            direction: FlowDirection::ReadWrite,
            operation: "http".to_string(),
        });
    }

    if contains_any(
        &target,
        &[
            "roomdatabase",
            "androidx.room",
            "sharedpreferences",
            "getsharedpreferences",
            "datastore",
            "sqlite",
            "mmkv",
            "realm",
            "contentresolver",
            ".putstring",
            ".putint",
            ".putlong",
            ".putboolean",
            ".getstring",
            ".getint",
            ".getlong",
            ".getboolean",
        ],
    ) {
        return Some(TerminalEffect {
            terminal_kind: TerminalKind::Persistence,
            direction: FlowDirection::ReadWrite,
            operation: "storage".to_string(),
        });
    }

    if contains_any(
        &target,
        &[
            "glide",
            "coil",
            "picasso",
            "imageloader",
            "lrucache",
            "disklrucache",
        ],
    ) {
        return Some(TerminalEffect {
            terminal_kind: TerminalKind::Cache,
            direction: FlowDirection::ReadWrite,
            operation: "cache".to_string(),
        });
    }

    if contains_any(
        &target,
        &[
            "startactivity",
            "startservice",
            "sendbroadcast",
            "navcontroller",
            ".navigate",
            "eventbus",
            "livedata.observe",
            "rxjava",
            ".subscribe",
            "workmanager",
        ],
    ) {
        return Some(TerminalEffect {
            terminal_kind: TerminalKind::Event,
            direction: FlowDirection::Write,
            operation: "event".to_string(),
        });
    }

    None
}

fn contains_any(value: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| value.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_android_network_calls() {
        let effect = terminal_effect_for_target("okHttpClient.newCall").unwrap();
        assert_eq!(effect.terminal_kind, TerminalKind::Network);
        assert_eq!(effect.operation, "http");
    }

    #[test]
    fn classifies_android_persistence_calls() {
        let effect = terminal_effect_for_target("getSharedPreferences").unwrap();
        assert_eq!(effect.terminal_kind, TerminalKind::Persistence);
        assert_eq!(effect.operation, "storage");
    }

    #[test]
    fn leaves_generic_create_unclassified() {
        assert!(terminal_effect_for_target("create").is_none());
    }
}
