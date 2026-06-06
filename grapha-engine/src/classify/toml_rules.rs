use regex::Regex;

use crate::classify::{Classification, Classifier, ClassifyContext};
use crate::config::ClassifierRule;
use grapha_core::graph::{FlowDirection, TerminalKind};

pub struct TomlRulesClassifier {
    compiled_rules: Vec<CompiledRule>,
}

struct CompiledRule {
    regex: Regex,
    terminal_kind: TerminalKind,
    direction: FlowDirection,
    operation: String,
}

impl TomlRulesClassifier {
    pub fn new(rules: &[ClassifierRule]) -> Self {
        let compiled_rules = rules
            .iter()
            .filter_map(|rule| {
                let regex = Regex::new(&rule.pattern).ok()?;
                let terminal_kind = parse_terminal_kind(&rule.terminal)?;
                let direction = parse_direction(&rule.direction)?;
                Some(CompiledRule {
                    regex,
                    terminal_kind,
                    direction,
                    operation: rule.operation.clone(),
                })
            })
            .collect();
        Self { compiled_rules }
    }
}

impl Classifier for TomlRulesClassifier {
    fn classify(&self, call_target: &str, _context: &ClassifyContext) -> Option<Classification> {
        self.compiled_rules.iter().find_map(|rule| {
            if rule.regex.is_match(call_target) {
                Some(Classification {
                    terminal_kind: rule.terminal_kind,
                    direction: rule.direction,
                    operation: rule.operation.clone(),
                })
            } else {
                None
            }
        })
    }
}

fn parse_terminal_kind(s: &str) -> Option<TerminalKind> {
    match s {
        "network" => Some(TerminalKind::Network),
        "persistence" => Some(TerminalKind::Persistence),
        "cache" => Some(TerminalKind::Cache),
        "event" => Some(TerminalKind::Event),
        "keychain" => Some(TerminalKind::Keychain),
        "search" => Some(TerminalKind::Search),
        _ => None,
    }
}

fn parse_direction(s: &str) -> Option<FlowDirection> {
    match s {
        "read" => Some(FlowDirection::Read),
        "write" => Some(FlowDirection::Write),
        "read_write" => Some(FlowDirection::ReadWrite),
        "pure" => Some(FlowDirection::Pure),
        _ => None,
    }
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
    fn matches_regex_pattern() {
        let rules = vec![ClassifierRule {
            pattern: r"Redis\.\w+".to_string(),
            terminal: "cache".to_string(),
            direction: "read_write".to_string(),
            operation: "Redis".to_string(),
        }];
        let c = TomlRulesClassifier::new(&rules);
        let result = c.classify("Redis.get", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Cache);
        assert_eq!(result.direction, FlowDirection::ReadWrite);
        assert_eq!(result.operation, "Redis");
    }

    #[test]
    fn returns_none_when_no_match() {
        let rules = vec![ClassifierRule {
            pattern: r"^Redis".to_string(),
            terminal: "cache".to_string(),
            direction: "read".to_string(),
            operation: "Redis".to_string(),
        }];
        let c = TomlRulesClassifier::new(&rules);
        let result = c.classify("Memcached.get", &ctx());
        assert!(result.is_none());
    }

    #[test]
    fn skips_invalid_regex() {
        let rules = vec![
            ClassifierRule {
                pattern: r"[invalid".to_string(),
                terminal: "cache".to_string(),
                direction: "read".to_string(),
                operation: "Bad".to_string(),
            },
            ClassifierRule {
                pattern: r"Redis".to_string(),
                terminal: "cache".to_string(),
                direction: "write".to_string(),
                operation: "Redis".to_string(),
            },
        ];
        let c = TomlRulesClassifier::new(&rules);
        // The invalid regex rule should be skipped, the second should work
        assert_eq!(c.compiled_rules.len(), 1);
        let result = c.classify("Redis.set", &ctx()).unwrap();
        assert_eq!(result.terminal_kind, TerminalKind::Cache);
        assert_eq!(result.direction, FlowDirection::Write);
    }

    #[test]
    fn skips_invalid_terminal_kind() {
        let rules = vec![ClassifierRule {
            pattern: r"Foo".to_string(),
            terminal: "nonsense".to_string(),
            direction: "read".to_string(),
            operation: "Foo".to_string(),
        }];
        let c = TomlRulesClassifier::new(&rules);
        assert!(c.compiled_rules.is_empty());
    }

    #[test]
    fn skips_invalid_direction() {
        let rules = vec![ClassifierRule {
            pattern: r"Bar".to_string(),
            terminal: "network".to_string(),
            direction: "nonsense".to_string(),
            operation: "Bar".to_string(),
        }];
        let c = TomlRulesClassifier::new(&rules);
        assert!(c.compiled_rules.is_empty());
    }

    #[test]
    fn first_matching_rule_wins() {
        let rules = vec![
            ClassifierRule {
                pattern: r"Redis".to_string(),
                terminal: "cache".to_string(),
                direction: "read".to_string(),
                operation: "FIRST".to_string(),
            },
            ClassifierRule {
                pattern: r"Redis".to_string(),
                terminal: "cache".to_string(),
                direction: "write".to_string(),
                operation: "SECOND".to_string(),
            },
        ];
        let c = TomlRulesClassifier::new(&rules);
        let result = c.classify("Redis.get", &ctx()).unwrap();
        assert_eq!(result.operation, "FIRST");
    }
}
