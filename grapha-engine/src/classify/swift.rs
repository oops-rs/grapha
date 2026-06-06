use crate::classify::{Classification, Classifier, ClassifyContext};
use grapha_core::graph::{FlowDirection, TerminalKind};

pub struct SwiftClassifier;

impl SwiftClassifier {
    pub fn new() -> Self {
        Self
    }
}

impl Classifier for SwiftClassifier {
    fn classify(&self, call_target: &str, _context: &ClassifyContext) -> Option<Classification> {
        classify_swift(call_target)
    }
}

fn classify_swift(target: &str) -> Option<Classification> {
    // Network
    if matches_any(target, &["URLSession", "AF.", "Alamofire", "Moya"]) {
        let direction = if contains_any(target, &["download", "data(", "get"]) {
            FlowDirection::Read
        } else if contains_any(target, &["upload", "post", "put", "delete"]) {
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

    // Persistence — CoreData
    if matches_any(
        target,
        &[
            "NSManagedObjectContext",
            "NSFetchRequest",
            "NSPersistentContainer",
        ],
    ) {
        let direction = if contains_any(target, &["fetch", "count"]) {
            FlowDirection::Read
        } else if contains_any(target, &["save", "delete", "insert"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Persistence,
            direction,
            operation: "CoreData".to_string(),
        });
    }

    // Persistence — Realm
    if matches_any(target, &["realm", "Realm"]) {
        let direction = if contains_any(target, &["objects", "object("]) {
            FlowDirection::Read
        } else if contains_any(target, &["write", "add", "delete"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Persistence,
            direction,
            operation: "Realm".to_string(),
        });
    }

    // Persistence — UserDefaults
    if matches_any(target, &["UserDefaults"]) {
        let direction = if contains_any(
            target,
            &["string(", "bool(", "integer(", "object(", "value("],
        ) {
            FlowDirection::Read
        } else if contains_any(target, &["set(", "setValue", "removeObject"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Persistence,
            direction,
            operation: "UserDefaults".to_string(),
        });
    }

    // Keychain
    if matches_any(target, &["KeychainWrapper", "SecItem", "Keychain"]) {
        let direction = if contains_any(target, &["get", "string(", "data(", "Copy"]) {
            FlowDirection::Read
        } else if contains_any(target, &["set", "add", "update", "delete", "remove"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Keychain,
            direction,
            operation: "Keychain".to_string(),
        });
    }

    // Event — NotificationCenter
    if matches_any(target, &["NotificationCenter"]) {
        let direction = if contains_any(target, &["post"]) {
            FlowDirection::Write
        } else if contains_any(target, &["addObserver", "observe"]) {
            FlowDirection::Read
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Event,
            direction,
            operation: if contains_any(target, &["post"]) {
                "publish".to_string()
            } else {
                "subscribe".to_string()
            },
        });
    }

    // Event — Combine subjects
    if matches_any(target, &["PassthroughSubject", "CurrentValueSubject"]) {
        return Some(Classification {
            terminal_kind: TerminalKind::Event,
            direction: FlowDirection::Write,
            operation: "publish".to_string(),
        });
    }

    // Cache
    if matches_any(target, &["NSCache"]) {
        let direction = if contains_any(target, &["object(", "value("]) {
            FlowDirection::Read
        } else if contains_any(target, &["setObject", "removeObject", "removeAll"]) {
            FlowDirection::Write
        } else {
            FlowDirection::ReadWrite
        };
        return Some(Classification {
            terminal_kind: TerminalKind::Cache,
            direction,
            operation: "Cache".to_string(),
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
            file: PathBuf::from("Test.swift"),
            arguments: vec![],
        }
    }

    #[test]
    fn classifies_urlsession_as_network() {
        let c = SwiftClassifier::new();
        let result = c.classify("URLSession.shared.dataTask", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Network);
        assert_eq!(result.operation, "HTTP");
    }

    #[test]
    fn classifies_alamofire_as_network() {
        let c = SwiftClassifier::new();
        let result = c.classify("AF.request", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Network);
    }

    #[test]
    fn classifies_moya_as_network() {
        let c = SwiftClassifier::new();
        let result = c.classify("MoyaProvider", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Network);
    }

    #[test]
    fn classifies_core_data_as_persistence() {
        let c = SwiftClassifier::new();
        let result = c.classify("NSManagedObjectContext.fetch", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "CoreData");
    }

    #[test]
    fn classifies_realm_as_persistence() {
        let c = SwiftClassifier::new();
        let result = c.classify("realm.write", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "Realm");
    }

    #[test]
    fn classifies_user_defaults_as_persistence() {
        let c = SwiftClassifier::new();
        let result = c
            .classify("UserDefaults.standard.set(value)", &ctx())
            .unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Persistence);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "UserDefaults");
    }

    #[test]
    fn classifies_keychain_as_keychain() {
        let c = SwiftClassifier::new();
        let result = c.classify("KeychainWrapper.standard.set", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Keychain);
        assert_eq!(result.direction, FlowDirection::Write);
    }

    #[test]
    fn classifies_secitem_as_keychain() {
        let c = SwiftClassifier::new();
        let result = c.classify("SecItemCopyMatching", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Keychain);
        assert_eq!(result.direction, FlowDirection::Read);
    }

    #[test]
    fn classifies_notification_center_post_as_event_publish() {
        let c = SwiftClassifier::new();
        let result = c
            .classify("NotificationCenter.default.post", &ctx())
            .unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Event);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "publish");
    }

    #[test]
    fn classifies_notification_center_observe_as_event_subscribe() {
        let c = SwiftClassifier::new();
        let result = c
            .classify("NotificationCenter.default.addObserver", &ctx())
            .unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Event);
        assert_eq!(result.direction, FlowDirection::Read);
        assert_eq!(result.operation, "subscribe");
    }

    #[test]
    fn classifies_passthrough_subject_as_event_publish() {
        let c = SwiftClassifier::new();
        let result = c.classify("PassthroughSubject.send", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Event);
        assert_eq!(result.direction, FlowDirection::Write);
        assert_eq!(result.operation, "publish");
    }

    #[test]
    fn classifies_current_value_subject_as_event_publish() {
        let c = SwiftClassifier::new();
        let result = c.classify("CurrentValueSubject.send", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Event);
        assert_eq!(result.direction, FlowDirection::Write);
    }

    #[test]
    fn classifies_nscache_as_cache() {
        let c = SwiftClassifier::new();
        let result = c.classify("NSCache.setObject", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Cache);
        assert_eq!(result.direction, FlowDirection::Write);
    }

    #[test]
    fn returns_none_for_unknown() {
        let c = SwiftClassifier::new();
        let result = c.classify("myCustomFunction", &ctx());
        assert!(result.is_none());
    }
}
