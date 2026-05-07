use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct SwiftConfig {
    #[serde(default = "default_true")]
    pub index_store: bool,
}

impl Default for SwiftConfig {
    fn default() -> Self {
        Self { index_store: true }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OutputConfig {
    #[serde(default)]
    pub default_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ArchitectureConfig {
    #[serde(default)]
    pub layers: Vec<ArchitectureLayer>,
    #[serde(default)]
    pub deny: Vec<ArchitectureDenyRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchitectureLayer {
    pub name: String,
    #[serde(default)]
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchitectureDenyRule {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExternalRepo {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RepoConfig {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InferredConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AnnotationConfig {
    #[serde(default)]
    pub server: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphaConfig {
    #[serde(default)]
    pub repo: RepoConfig,
    #[serde(default)]
    pub swift: SwiftConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub architecture: ArchitectureConfig,
    #[serde(default)]
    pub inferred: InferredConfig,
    #[serde(default)]
    pub annotations: AnnotationConfig,
    #[serde(default)]
    pub classifiers: Vec<ClassifierRule>,
    #[serde(default)]
    pub external: Vec<ExternalRepo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClassifierRule {
    pub pattern: String,
    pub terminal: String,
    pub direction: String,
    pub operation: String,
}

impl GraphaConfig {
    pub fn extraction_cache_fingerprint(&self) -> String {
        #[derive(Serialize)]
        struct ExtractionCacheFingerprint<'a> {
            swift_index_store: bool,
            classifiers: &'a [ClassifierRule],
        }

        serde_json::to_string(&ExtractionCacheFingerprint {
            swift_index_store: self.swift.index_store,
            classifiers: &self.classifiers,
        })
        .unwrap_or_default()
    }

    pub fn index_input_fingerprint(&self) -> String {
        #[derive(Serialize)]
        struct IndexInputFingerprint<'a> {
            repo_name: &'a Option<String>,
            swift_index_store: bool,
            classifiers: &'a [ClassifierRule],
            external: &'a [ExternalRepo],
        }

        serde_json::to_string(&IndexInputFingerprint {
            repo_name: &self.repo.name,
            swift_index_store: self.swift.index_store,
            classifiers: &self.classifiers,
            external: &self.external,
        })
        .unwrap_or_default()
    }
}

pub fn load_config(project_root: &Path) -> GraphaConfig {
    let config_path = project_root.join("grapha.toml");
    if !config_path.exists() {
        return GraphaConfig::default();
    }
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => GraphaConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn parse_empty_config() {
        let config: GraphaConfig = toml::from_str("").unwrap();
        assert!(config.classifiers.is_empty());
        assert!(config.swift.index_store);
    }

    #[test]
    fn parse_classifier_rules() {
        let toml_str = r#"
[[classifiers]]
pattern = "URLSession"
terminal = "network"
direction = "read"
operation = "HTTP_GET"

[[classifiers]]
pattern = "CoreData"
terminal = "persistence"
direction = "write"
operation = "INSERT"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.classifiers.len(), 2);
        assert_eq!(config.classifiers[0].pattern, "URLSession");
        assert_eq!(config.classifiers[0].terminal, "network");
        assert_eq!(config.classifiers[0].direction, "read");
        assert_eq!(config.classifiers[0].operation, "HTTP_GET");
        assert_eq!(config.classifiers[1].pattern, "CoreData");
        assert_eq!(config.classifiers[1].terminal, "persistence");
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let config = load_config(dir.path());
        assert!(config.classifiers.is_empty());
    }

    #[test]
    fn load_from_file_works() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("grapha.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        writeln!(
            f,
            r#"
[[classifiers]]
pattern = "reqwest"
terminal = "network"
direction = "read_write"
operation = "HTTP"
"#
        )
        .unwrap();

        let config = load_config(dir.path());
        assert_eq!(config.classifiers.len(), 1);
        assert_eq!(config.classifiers[0].pattern, "reqwest");
    }

    #[test]
    fn swift_index_store_disabled() {
        let toml_str = r#"
[swift]
index_store = false
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.swift.index_store);
    }

    #[test]
    fn swift_defaults_when_only_classifiers() {
        let toml_str = r#"
[[classifiers]]
pattern = "Alamofire"
terminal = "network"
direction = "read"
operation = "HTTP"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert!(config.swift.index_store);
        assert_eq!(config.classifiers.len(), 1);
    }

    #[test]
    fn swift_index_store_defaults_true_when_section_empty() {
        let toml_str = "[swift]\n";
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert!(config.swift.index_store);
    }

    #[test]
    fn parse_external_repos() {
        let toml_str = r#"
[[external]]
name = "FrameUI"
path = "/path/to/frameui"

[[external]]
name = "FrameNetwork"
path = "/path/to/framenetwork"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.external.len(), 2);
        assert_eq!(config.external[0].name, "FrameUI");
        assert_eq!(config.external[0].path, "/path/to/frameui");
        assert_eq!(config.external[1].name, "FrameNetwork");
        assert_eq!(config.external[1].path, "/path/to/framenetwork");
    }

    #[test]
    fn external_defaults_empty() {
        let config: GraphaConfig = toml::from_str("").unwrap();
        assert!(config.external.is_empty());
    }

    #[test]
    fn parse_repo_name() {
        let toml_str = r#"
[repo]
name = "MobileApp"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.repo.name.as_deref(), Some("MobileApp"));
    }

    #[test]
    fn parse_annotation_server() {
        let toml_str = r#"
[annotations]
server = "http://192.168.1.10:8080"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.annotations.server.as_deref(),
            Some("http://192.168.1.10:8080")
        );
    }

    #[test]
    fn parse_architecture_rules() {
        let toml_str = r#"
[[architecture.layers]]
name = "ui"
patterns = ["AppUI*", "Features/*/View*"]

[[architecture.layers]]
name = "infra"
patterns = ["Networking*", "Persistence*"]

[[architecture.deny]]
from = "infra"
to = "ui"
reason = "Infrastructure must not depend on UI."
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.architecture.layers.len(), 2);
        assert_eq!(config.architecture.layers[0].name, "ui");
        assert_eq!(
            config.architecture.layers[0].patterns,
            ["AppUI*", "Features/*/View*"]
        );
        assert_eq!(config.architecture.deny.len(), 1);
        assert_eq!(config.architecture.deny[0].from, "infra");
        assert_eq!(config.architecture.deny[0].to, "ui");
        assert_eq!(
            config.architecture.deny[0].reason.as_deref(),
            Some("Infrastructure must not depend on UI.")
        );
    }

    #[test]
    fn architecture_defaults_empty() {
        let config: GraphaConfig = toml::from_str("").unwrap();
        assert!(config.architecture.layers.is_empty());
        assert!(config.architecture.deny.is_empty());
    }

    #[test]
    fn inferred_defaults_disabled() {
        let config: GraphaConfig = toml::from_str("").unwrap();
        assert!(!config.inferred.enabled);
    }

    #[test]
    fn parse_inferred_config() {
        let config: GraphaConfig = toml::from_str(
            r#"
[inferred]
enabled = true
"#,
        )
        .unwrap();
        assert!(config.inferred.enabled);
    }

    #[test]
    fn extraction_cache_fingerprint_tracks_only_extraction_settings() {
        let config_a: GraphaConfig = toml::from_str(
            r#"
[[classifiers]]
pattern = "reqwest"
terminal = "network"
direction = "read"
operation = "HTTP"

[output]
default_fields = ["id"]
"#,
        )
        .unwrap();
        let config_b: GraphaConfig = toml::from_str(
            r#"
[[classifiers]]
pattern = "reqwest"
terminal = "network"
direction = "read"
operation = "HTTP"

[output]
default_fields = ["id", "file"]
"#,
        )
        .unwrap();
        let config_c: GraphaConfig = toml::from_str(
            r#"
[[classifiers]]
pattern = "reqwest"
terminal = "event"
direction = "write"
operation = "PUBLISH"
"#,
        )
        .unwrap();

        assert_eq!(
            config_a.extraction_cache_fingerprint(),
            config_b.extraction_cache_fingerprint()
        );
        assert_ne!(
            config_a.extraction_cache_fingerprint(),
            config_c.extraction_cache_fingerprint()
        );
    }

    #[test]
    fn index_input_fingerprint_tracks_external_repos() {
        let config_a: GraphaConfig = toml::from_str(
            r#"
[[external]]
name = "Shared"
path = "../shared"
"#,
        )
        .unwrap();
        let config_b: GraphaConfig = toml::from_str("").unwrap();

        assert_ne!(
            config_a.index_input_fingerprint(),
            config_b.index_input_fingerprint()
        );
    }

    #[test]
    fn index_input_fingerprint_tracks_repo_name() {
        let config_a: GraphaConfig = toml::from_str(
            r#"
[repo]
name = "AppA"
"#,
        )
        .unwrap();
        let config_b: GraphaConfig = toml::from_str(
            r#"
[repo]
name = "AppB"
"#,
        )
        .unwrap();

        assert_ne!(
            config_a.index_input_fingerprint(),
            config_b.index_input_fingerprint()
        );
    }
}
