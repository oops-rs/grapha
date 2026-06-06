use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

fn default_remote_channel() -> String {
    "default".to_string()
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
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub index_path: Option<String>,
    #[serde(default)]
    pub remote: Option<ExternalRemoteBaseline>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalRemoteBaseline {
    pub project_id: String,
    #[serde(default = "default_remote_channel")]
    pub channel: String,
    #[serde(default)]
    pub server: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RepoConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
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
pub struct ServeConfig {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub watch: Option<bool>,
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
    pub serve: ServeConfig,
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
            repo_project_id: &'a Option<String>,
            swift_index_store: bool,
            classifiers: &'a [ClassifierRule],
            external: &'a [ExternalRepo],
        }

        serde_json::to_string(&IndexInputFingerprint {
            repo_name: &self.repo.name,
            repo_project_id: &self.repo.project_id,
            swift_index_store: self.swift.index_store,
            classifiers: &self.classifiers,
            external: &self.external,
        })
        .unwrap_or_default()
    }
}

pub fn load_config(project_root: &Path) -> GraphaConfig {
    let config_path = project_root.join("grapha.toml");
    load_config_file(&config_path)
}

pub fn load_global_config() -> GraphaConfig {
    load_first_existing_config(global_config_paths())
}

pub fn default_config_dir() -> PathBuf {
    default_config_dir_from_paths(global_config_paths())
}

pub fn default_annotation_log_path() -> PathBuf {
    default_config_dir().join("annotation-service.log")
}

fn default_config_dir_from_paths(paths: Vec<PathBuf>) -> PathBuf {
    paths
        .into_iter()
        .next()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from(".grapha"))
}

pub fn global_config_paths() -> Vec<PathBuf> {
    global_config_paths_from_env(
        |key| std::env::var_os(key),
        home_dir_from_env,
        current_platform,
    )
}

fn load_first_existing_config(paths: Vec<PathBuf>) -> GraphaConfig {
    paths
        .into_iter()
        .find(|path| path.exists())
        .map(|path| load_config_file(&path))
        .unwrap_or_default()
}

fn load_config_file(config_path: &Path) -> GraphaConfig {
    if !config_path.exists() {
        return GraphaConfig::default();
    }
    match std::fs::read_to_string(config_path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => GraphaConfig::default(),
    }
}

fn global_config_paths_from_env<E, H, P>(mut env_var: E, home_dir: H, platform: P) -> Vec<PathBuf>
where
    E: FnMut(&str) -> Option<OsString>,
    H: Fn() -> Option<PathBuf>,
    P: Fn() -> Platform,
{
    if let Some(path) = env_var("GRAPHA_CONFIG").filter(|value| !value.is_empty()) {
        return vec![PathBuf::from(path)];
    }

    let mut paths = Vec::new();
    match platform() {
        Platform::Windows => {
            if let Some(appdata) = env_var("APPDATA").filter(|value| !value.is_empty()) {
                paths.push(PathBuf::from(appdata).join("grapha").join("config.toml"));
            }
            if let Some(home) = home_dir() {
                paths.push(home.join(".config").join("grapha").join("config.toml"));
                paths.push(home.join(".grapha").join("config.toml"));
            }
        }
        Platform::Unix | Platform::MacOs => {
            if let Some(xdg_config_home) =
                env_var("XDG_CONFIG_HOME").filter(|value| !value.is_empty())
            {
                paths.push(
                    PathBuf::from(xdg_config_home)
                        .join("grapha")
                        .join("config.toml"),
                );
            } else if let Some(home) = home_dir() {
                paths.push(home.join(".config").join("grapha").join("config.toml"));
            }
            if let Some(home) = home_dir() {
                paths.push(home.join(".grapha").join("config.toml"));
            }
        }
    }

    paths.dedup();
    paths
}

fn home_dir_from_env() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }

    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Platform {
    MacOs,
    Windows,
    Unix,
}

fn current_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::MacOs
    }

    #[cfg(windows)]
    {
        Platform::Windows
    }

    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        Platform::Unix
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
        assert_eq!(config.external[0].path.as_deref(), Some("/path/to/frameui"));
        assert_eq!(config.external[1].name, "FrameNetwork");
        assert_eq!(
            config.external[1].path.as_deref(),
            Some("/path/to/framenetwork")
        );
    }

    #[test]
    fn external_defaults_empty() {
        let config: GraphaConfig = toml::from_str("").unwrap();
        assert!(config.external.is_empty());
    }

    #[test]
    fn parse_external_remote_baseline() {
        let toml_str = r#"
[[external]]
name = "FrameUI"
index_path = "/indexes/frameui/.grapha"

[external.remote]
project_id = "remote-frameui"
server = "http://127.0.0.1:8080"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        let external = &config.external[0];

        assert_eq!(external.name, "FrameUI");
        assert_eq!(external.path, None);
        assert_eq!(
            external.index_path.as_deref(),
            Some("/indexes/frameui/.grapha")
        );
        assert_eq!(
            external
                .remote
                .as_ref()
                .map(|remote| remote.project_id.as_str()),
            Some("remote-frameui")
        );
        assert_eq!(
            external
                .remote
                .as_ref()
                .map(|remote| remote.channel.as_str()),
            Some("default")
        );
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
    fn parse_repo_project_id() {
        let toml_str = r#"
[repo]
name = "MobileApp"
project_id = "mobile-app-prod"
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.repo.project_id.as_deref(), Some("mobile-app-prod"));
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
    fn default_annotation_log_path_uses_config_directory() {
        let config_dir = default_config_dir_from_paths(vec![PathBuf::from(
            "/home/dev/.config/grapha/config.toml",
        )]);

        assert_eq!(config_dir, PathBuf::from("/home/dev/.config/grapha"));
        assert_eq!(
            config_dir.join("annotation-service.log"),
            PathBuf::from("/home/dev/.config/grapha/annotation-service.log")
        );
    }

    #[test]
    fn parse_serve_config() {
        let toml_str = r#"
[serve]
host = "127.0.0.1"
port = 18081
watch = true
"#;
        let config: GraphaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.serve.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.serve.port, Some(18081));
        assert_eq!(config.serve.watch, Some(true));
    }

    #[test]
    fn global_config_paths_prefer_explicit_grapha_config() {
        let explicit = PathBuf::from("/tmp/grapha-global.toml");
        let paths = global_config_paths_from_env(
            |key| (key == "GRAPHA_CONFIG").then(|| explicit.clone().into_os_string()),
            || Some(PathBuf::from("/home/dev")),
            || Platform::Unix,
        );

        assert_eq!(paths, vec![explicit]);
    }

    #[test]
    fn global_config_paths_use_xdg_config_then_home_grapha_on_unix() {
        let xdg_config_home = PathBuf::from("/tmp/xdg-config");
        let home = PathBuf::from("/home/dev");
        let paths = global_config_paths_from_env(
            |key| (key == "XDG_CONFIG_HOME").then(|| xdg_config_home.clone().into_os_string()),
            || Some(home.clone()),
            || Platform::Unix,
        );

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/tmp/xdg-config/grapha/config.toml"),
                PathBuf::from("/home/dev/.grapha/config.toml")
            ]
        );
    }

    #[test]
    fn global_config_paths_use_home_config_when_xdg_is_missing() {
        let home = PathBuf::from("/home/dev");
        let paths =
            global_config_paths_from_env(|_| None, || Some(home.clone()), || Platform::Unix);

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/dev/.config/grapha/config.toml"),
                PathBuf::from("/home/dev/.grapha/config.toml")
            ]
        );
    }

    #[test]
    fn load_first_existing_global_config_reads_annotations_server() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.toml");
        let config_path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        writeln!(
            f,
            r#"
[annotations]
server = "http://10.0.0.2:8080"
"#
        )
        .unwrap();

        let config = load_first_existing_config(vec![missing, config_path]);

        assert_eq!(
            config.annotations.server.as_deref(),
            Some("http://10.0.0.2:8080")
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
