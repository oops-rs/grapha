use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use grapha_core::graph::Graph;
use serde::{Deserialize, Serialize};
use tantivy::Index;

pub const DEFAULT_CHANNEL: &str = "default";
pub const PUBLISH_BUNDLE_SCHEMA_VERSION: u32 = 1;
const BRANCH_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const BRANCH_MAX_REVISIONS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRevisionMetadata {
    pub project_id: String,
    pub repo_name: String,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    pub config_fingerprint: String,
    pub graph_version: String,
    pub grapha_version: String,
    pub bundle_schema_version: u32,
    pub published_at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectIndexBundle {
    pub metadata: ProjectRevisionMetadata,
    pub graph: Graph,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRevisionSummary {
    pub revision_id: String,
    pub metadata: ProjectRevisionMetadata,
    pub node_count: usize,
    pub edge_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStorageClass {
    Default,
    Release,
    Branch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelStoragePolicy {
    pub class: ChannelStorageClass,
    pub durable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_revisions: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProjectChannelPointer {
    revision_id: String,
    metadata: ProjectRevisionMetadata,
    node_count: usize,
    edge_count: usize,
}

pub struct ProjectRevisionStore {
    data_root: PathBuf,
}

impl ProjectRevisionStore {
    pub fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    pub fn publish(&self, bundle: &ProjectIndexBundle) -> anyhow::Result<ProjectRevisionSummary> {
        validate_bundle(bundle)?;
        let project_dir = self.project_dir(&bundle.metadata.project_id)?;
        let revision_id = revision_id(&bundle.metadata);
        let revision_dir = project_dir.join("revisions").join(&revision_id);
        let bundle_path = revision_dir.join("bundle.json");
        let search_index_path = revision_dir.join("search_index");

        std::fs::create_dir_all(&revision_dir)?;
        std::fs::write(&bundle_path, serde_json::to_vec_pretty(bundle)?)?;
        crate::search::build_index(&bundle.graph, &search_index_path)?;

        let summary = ProjectRevisionSummary {
            revision_id,
            metadata: bundle.metadata.clone(),
            node_count: bundle.graph.nodes.len(),
            edge_count: bundle.graph.edges.len(),
        };
        let pointer = ProjectChannelPointer {
            revision_id: summary.revision_id.clone(),
            metadata: summary.metadata.clone(),
            node_count: summary.node_count,
            edge_count: summary.edge_count,
        };
        let pointer_path = channel_pointer_path(&project_dir, &bundle.metadata.channel);
        write_json_atomic(&pointer_path, &pointer)?;
        Ok(summary)
    }

    pub fn load_bundle(
        &self,
        project_id: &str,
        channel: &str,
    ) -> anyhow::Result<ProjectIndexBundle> {
        let pointer = self.load_pointer(project_id, channel)?;
        let bundle_path = self
            .project_dir(project_id)?
            .join("revisions")
            .join(pointer.revision_id)
            .join("bundle.json");
        Ok(serde_json::from_slice(&std::fs::read(bundle_path)?)?)
    }

    pub fn summary(
        &self,
        project_id: &str,
        channel: &str,
    ) -> anyhow::Result<ProjectRevisionSummary> {
        let pointer = self.load_pointer(project_id, channel)?;
        Ok(ProjectRevisionSummary {
            revision_id: pointer.revision_id,
            metadata: pointer.metadata,
            node_count: pointer.node_count,
            edge_count: pointer.edge_count,
        })
    }

    pub fn open_search_index(&self, project_id: &str, channel: &str) -> anyhow::Result<Index> {
        let pointer = self.load_pointer(project_id, channel)?;
        let search_index_path = self
            .project_dir(project_id)?
            .join("revisions")
            .join(pointer.revision_id)
            .join("search_index");
        Ok(Index::open_in_dir(search_index_path)?)
    }

    fn load_pointer(
        &self,
        project_id: &str,
        channel: &str,
    ) -> anyhow::Result<ProjectChannelPointer> {
        let project_dir = self.project_dir(project_id)?;
        let pointer_path = channel_pointer_path(&project_dir, channel);
        Ok(serde_json::from_slice(&std::fs::read(pointer_path)?)?)
    }

    fn project_dir(&self, project_id: &str) -> anyhow::Result<PathBuf> {
        let project_id = crate::data_paths::validate_project_id(project_id)?;
        Ok(self.data_root.join("projects").join(project_id))
    }
}

pub fn build_publish_bundle(
    project_root: &Path,
    graph: Graph,
    channel: &str,
    config_fingerprint: String,
) -> anyhow::Result<ProjectIndexBundle> {
    let channel = non_empty(channel).unwrap_or(DEFAULT_CHANNEL);
    let identity = crate::data_paths::project_identity(project_root);
    let repo_name = crate::data_paths::repo_name_for_project_root(project_root);
    let metadata = ProjectRevisionMetadata {
        project_id: identity.project_id,
        repo_name,
        channel: channel.to_string(),
        head_oid: identity.head_oid,
        head_ref: identity.head_ref,
        config_fingerprint,
        graph_version: graph.version.clone(),
        grapha_version: env!("CARGO_PKG_VERSION").to_string(),
        bundle_schema_version: PUBLISH_BUNDLE_SCHEMA_VERSION,
        published_at_unix_secs: current_unix_secs(),
    };
    let bundle = ProjectIndexBundle { metadata, graph };
    validate_bundle(&bundle)?;
    Ok(bundle)
}

pub fn validate_bundle(bundle: &ProjectIndexBundle) -> anyhow::Result<()> {
    crate::data_paths::validate_project_id(&bundle.metadata.project_id)?;
    if non_empty(&bundle.metadata.repo_name).is_none() {
        anyhow::bail!("repo_name cannot be empty");
    }
    if non_empty(&bundle.metadata.channel).is_none() {
        anyhow::bail!("channel cannot be empty");
    }
    if bundle.metadata.bundle_schema_version != PUBLISH_BUNDLE_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported publish bundle schema {}; expected {}",
            bundle.metadata.bundle_schema_version,
            PUBLISH_BUNDLE_SCHEMA_VERSION
        );
    }
    if bundle.metadata.graph_version != bundle.graph.version {
        anyhow::bail!("bundle graph_version does not match graph.version");
    }
    Ok(())
}

pub fn channel_policy(channel: &str) -> ChannelStoragePolicy {
    let channel = channel.trim();
    if channel == DEFAULT_CHANNEL {
        return ChannelStoragePolicy {
            class: ChannelStorageClass::Default,
            durable: true,
            ttl_seconds: None,
            max_revisions: None,
        };
    }
    if channel == "release" || channel.starts_with("release/") {
        return ChannelStoragePolicy {
            class: ChannelStorageClass::Release,
            durable: true,
            ttl_seconds: None,
            max_revisions: None,
        };
    }
    ChannelStoragePolicy {
        class: ChannelStorageClass::Branch,
        durable: false,
        ttl_seconds: Some(BRANCH_TTL_SECONDS),
        max_revisions: Some(BRANCH_MAX_REVISIONS),
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn current_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn revision_id(metadata: &ProjectRevisionMetadata) -> String {
    let head = metadata
        .head_oid
        .as_deref()
        .map(|oid| oid.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "nohead".to_string());
    format!(
        "{}-{}-{head}",
        safe_path_component(&metadata.channel),
        current_unix_millis()
    )
}

fn channel_pointer_path(project_dir: &Path, channel: &str) -> PathBuf {
    project_dir
        .join("channels")
        .join(format!("{}.json", safe_path_component(channel)))
}

fn safe_path_component(value: &str) -> String {
    let mut safe = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            safe.push(byte as char);
        } else if !safe.ends_with('_') {
            safe.push('_');
        }
    }
    let safe = safe.trim_matches('_');
    if safe.is_empty() {
        "default".to_string()
    } else {
        safe.to_string()
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::Graph;

    fn bundle(project_id: &str, channel: &str) -> ProjectIndexBundle {
        let graph = Graph::new();
        ProjectIndexBundle {
            metadata: ProjectRevisionMetadata {
                project_id: project_id.to_string(),
                repo_name: "Demo".to_string(),
                channel: channel.to_string(),
                head_oid: Some("1234567890abcdef".to_string()),
                head_ref: Some("main".to_string()),
                config_fingerprint: "{}".to_string(),
                graph_version: graph.version.clone(),
                grapha_version: "0.0.0-test".to_string(),
                bundle_schema_version: PUBLISH_BUNDLE_SCHEMA_VERSION,
                published_at_unix_secs: 1,
            },
            graph,
        }
    }

    #[test]
    fn storage_policy_keeps_default_and_release_durable() {
        assert_eq!(
            channel_policy("default").class,
            ChannelStorageClass::Default
        );
        assert!(channel_policy("default").durable);
        assert_eq!(
            channel_policy("release/1.0").class,
            ChannelStorageClass::Release
        );
        assert!(channel_policy("release/1.0").durable);
    }

    #[test]
    fn storage_policy_limits_branch_channels() {
        let policy = channel_policy("feature/room-page");

        assert_eq!(policy.class, ChannelStorageClass::Branch);
        assert!(!policy.durable);
        assert_eq!(policy.ttl_seconds, Some(BRANCH_TTL_SECONDS));
        assert_eq!(policy.max_revisions, Some(BRANCH_MAX_REVISIONS));
    }

    #[test]
    fn rejects_incompatible_bundle_schema() {
        let mut bundle = bundle("demo-project", "default");
        bundle.metadata.bundle_schema_version = PUBLISH_BUNDLE_SCHEMA_VERSION + 1;

        let error = validate_bundle(&bundle).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported publish bundle schema")
        );
    }

    #[test]
    fn publish_promotes_channel_after_graph_and_search_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectRevisionStore::new(dir.path().to_path_buf());
        let bundle = bundle("demo-project", "default");

        let summary = store.publish(&bundle).unwrap();
        let loaded = store.load_bundle("demo-project", "default").unwrap();
        let search = store.open_search_index("demo-project", "default");

        assert_eq!(summary.metadata.project_id, "demo-project");
        assert_eq!(loaded.metadata.project_id, "demo-project");
        assert!(search.is_ok());
    }
}
