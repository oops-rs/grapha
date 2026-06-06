use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use git2::{DiffOptions, Oid, Repository, StatusOptions};
use serde::{Deserialize, Serialize};

use crate::cache::{self, FileStamp};
use crate::config::GraphaConfig;
use crate::{assets, localization};

const INDEX_STATUS_FILENAME: &str = "index_status.json";
const INDEX_STATUS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedRepoFile {
    path: String,
    stamp: Option<FileStamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedRepoState {
    root: String,
    head_oid: Option<String>,
    head_ref: Option<String>,
    dirty_files: Vec<IndexedRepoFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexStatusSnapshot {
    version: u32,
    indexed_at_unix_secs: u64,
    grapha_version: String,
    node_count: usize,
    edge_count: usize,
    #[serde(default)]
    binary_stamp: Option<FileStamp>,
    #[serde(default)]
    config_fingerprint: String,
    #[serde(default)]
    index_store_path: Option<String>,
    #[serde(default)]
    index_store_stamp: Option<FileStamp>,
    repo: Option<IndexedRepoState>,
    #[serde(default)]
    borrowed_from: Option<BorrowedIndexSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BorrowedIndexSource {
    project_root: String,
    store_dir: String,
    migrated_at_unix_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_head_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_head_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_head_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_head_ref: Option<String>,
    pub changed_file_count_since_index: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files_since_index: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub indexed_at_unix_secs: u64,
    pub grapha_version: String,
    pub node_count: usize,
    pub edge_count: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    pub temporary: bool,
    pub may_be_stale: bool,
    pub freshness_tracking_available: bool,
    pub changed_file_count_since_index: usize,
    pub changed_input_file_count_since_index: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_input_files_since_index: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub borrowed_from: Option<BorrowedIndexStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BorrowedIndexStatus {
    pub project_root: String,
    pub store_dir: String,
    pub migrated_at_unix_secs: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct IndexInputKinds {
    graph: bool,
    localization: bool,
    assets: bool,
}

#[derive(Debug, Clone)]
pub struct IndexWorkPlan {
    pub status: IndexStatus,
    pub rebuild_graph: bool,
    pub rebuild_localization: bool,
    pub rebuild_assets: bool,
}

impl IndexWorkPlan {
    pub fn is_noop(&self) -> bool {
        !self.rebuild_graph && !self.rebuild_localization && !self.rebuild_assets
    }
}

fn normalize_repo_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_store_artifact(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".grapha")
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn classify_index_input(path: &str) -> IndexInputKinds {
    let path = Path::new(path);
    let file_name = path.file_name().and_then(|value| value.to_str());

    if file_name == Some("grapha.toml") {
        return IndexInputKinds {
            graph: true,
            localization: false,
            assets: false,
        };
    }

    if file_name == Some("langcodec.toml") {
        return IndexInputKinds {
            graph: false,
            localization: true,
            assets: false,
        };
    }

    if file_name == Some("Package.swift") || file_name == Some("Cargo.toml") {
        return IndexInputKinds {
            graph: true,
            localization: false,
            assets: false,
        };
    }

    if path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|segment| {
            segment.ends_with(".xcodeproj") || segment.ends_with(".xcworkspace")
        })
    }) {
        return IndexInputKinds {
            graph: true,
            localization: false,
            assets: false,
        };
    }

    if path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|segment| segment.ends_with(".xcassets"))
    }) {
        return IndexInputKinds {
            graph: false,
            localization: false,
            assets: true,
        };
    }

    match path.extension().and_then(|value| value.to_str()) {
        Some("swift") | Some("rs") => IndexInputKinds {
            graph: true,
            localization: false,
            assets: false,
        },
        Some("xcstrings") | Some("strings") => IndexInputKinds {
            graph: false,
            localization: true,
            assets: false,
        },
        _ => IndexInputKinds::default(),
    }
}

fn collect_changed_input_files(changed_files: &BTreeSet<String>) -> Vec<String> {
    changed_files
        .iter()
        .filter(|path| {
            let kinds = classify_index_input(path);
            kinds.graph || kinds.localization || kinds.assets
        })
        .cloned()
        .collect()
}

fn path_mtime_unix_secs(path: &Path) -> anyhow::Result<u64> {
    Ok(fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs())
}

fn head_state(repo: &Repository) -> (Option<String>, Option<String>) {
    let head = repo.head().ok();
    let head_oid = head
        .as_ref()
        .and_then(|head| head.target())
        .map(|oid| oid.to_string());
    let head_ref = head
        .as_ref()
        .and_then(|head| head.shorthand())
        .map(str::to_string);
    (head_oid, head_ref)
}

fn repo_root(repo: &Repository) -> Option<PathBuf> {
    repo.workdir()
        .map(Path::to_path_buf)
        .or_else(|| repo.path().parent().map(Path::to_path_buf))
}

fn dirty_repo_files(repo: &Repository) -> anyhow::Result<Vec<IndexedRepoFile>> {
    let Some(root) = repo_root(repo) else {
        return Ok(Vec::new());
    };

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut files = BTreeMap::new();
    for entry in statuses.iter() {
        let Some(path) = entry.path() else {
            continue;
        };
        let relative = PathBuf::from(path);
        if is_store_artifact(&relative) {
            continue;
        }
        let stamp = FileStamp::from_path(&root.join(&relative));
        files.insert(
            normalize_repo_path(&relative),
            IndexedRepoFile {
                path: normalize_repo_path(&relative),
                stamp,
            },
        );
    }

    Ok(files.into_values().collect())
}

fn capture_repo_state(project_root: &Path) -> anyhow::Result<Option<IndexedRepoState>> {
    let repo = match Repository::discover(project_root) {
        Ok(repo) => repo,
        Err(_) => return Ok(None),
    };
    let Some(root) = repo_root(&repo) else {
        return Ok(None);
    };
    let (head_oid, head_ref) = head_state(&repo);
    Ok(Some(IndexedRepoState {
        root: normalize_repo_path(&root),
        head_oid,
        head_ref,
        dirty_files: dirty_repo_files(&repo)?,
    }))
}

fn status_path(store_dir: &Path) -> PathBuf {
    store_dir.join(INDEX_STATUS_FILENAME)
}

fn required_index_artifacts_exist(store_dir: &Path) -> bool {
    store_dir.join("grapha.db").is_file()
        && store_dir.join("search_index").is_dir()
        && localization::snapshot_exists(store_dir)
        && assets::snapshot_exists(store_dir)
}

fn current_index_store_info(
    project_root: &Path,
    config: &GraphaConfig,
) -> (Option<String>, Option<FileStamp>) {
    if !config.swift.index_store {
        return (None, None);
    }

    #[cfg(feature = "swift")]
    let path = grapha_swift::refresh_index_store(project_root);
    #[cfg(not(feature = "swift"))]
    let path: Option<PathBuf> = {
        let _ = project_root;
        None
    };
    let stamp = path.as_deref().and_then(FileStamp::from_path);
    let path = path.map(|path| normalize_repo_path(&path));
    (path, stamp)
}

fn snapshot_index_store_compatible(
    snapshot: &IndexStatusSnapshot,
    project_root: &Path,
    config: &GraphaConfig,
) -> bool {
    if !config.swift.index_store {
        return snapshot.index_store_path.is_none();
    }

    if let Some(snapshot_path) = snapshot.index_store_path.as_deref() {
        let current_stamp = FileStamp::from_path(Path::new(snapshot_path));
        if current_stamp == snapshot.index_store_stamp && current_stamp.is_some() {
            return true;
        }
    }

    let (current_path, current_stamp) = current_index_store_info(project_root, config);
    current_path == snapshot.index_store_path && current_stamp == snapshot.index_store_stamp
}

fn legacy_snapshot_compatible(
    snapshot: &IndexStatusSnapshot,
    project_root: &Path,
    store_dir: &Path,
    config: &GraphaConfig,
) -> bool {
    if snapshot.grapha_version != env!("CARGO_PKG_VERSION") {
        return false;
    }

    let cache = crate::cache::ExtractionCache::new(store_dir);
    let Ok(entries) = cache.load_entries() else {
        return false;
    };
    if entries.is_empty() {
        return false;
    }

    let expected_fingerprint = config.extraction_cache_fingerprint();
    if entries
        .values()
        .any(|entry| entry.config_fingerprint != expected_fingerprint)
    {
        return false;
    }

    if config.swift.index_store {
        current_index_store_info(project_root, config).0.is_some()
    } else {
        true
    }
}

fn save_snapshot(store_dir: &Path, snapshot: &IndexStatusSnapshot) -> anyhow::Result<()> {
    if let Some(parent) = store_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(store_dir)?;
    let payload = serde_json::to_string_pretty(snapshot)?;
    fs::write(status_path(store_dir), payload)
        .with_context(|| format!("writing {}", status_path(store_dir).display()))
}

fn load_snapshot(store_dir: &Path) -> anyhow::Result<IndexStatusSnapshot> {
    let payload = fs::read_to_string(status_path(store_dir))
        .with_context(|| format!("reading {}", status_path(store_dir).display()))?;
    let snapshot: IndexStatusSnapshot = serde_json::from_str(&payload)
        .with_context(|| format!("parsing {}", status_path(store_dir).display()))?;
    if snapshot.version != INDEX_STATUS_VERSION {
        anyhow::bail!(
            "unsupported index status version: {} (expected {})",
            snapshot.version,
            INDEX_STATUS_VERSION
        );
    }
    Ok(snapshot)
}

fn legacy_status(store_dir: &Path) -> anyhow::Result<IndexStatus> {
    let db_path = store_dir.join("grapha.db");
    if !db_path.exists() {
        anyhow::bail!("no index found — run `grapha index` first");
    }

    Ok(IndexStatus {
        indexed_at_unix_secs: path_mtime_unix_secs(&db_path)?,
        grapha_version: env!("CARGO_PKG_VERSION").to_string(),
        node_count: 0,
        edge_count: 0,
        temporary: false,
        may_be_stale: false,
        freshness_tracking_available: false,
        changed_file_count_since_index: 0,
        changed_input_file_count_since_index: 0,
        changed_input_files_since_index: Vec::new(),
        repo: None,
        borrowed_from: None,
        note: Some(
            "reindex with the current Grapha build to enable freshness tracking".to_string(),
        ),
    })
}

fn current_dirty_file_map(
    repo: &Repository,
) -> anyhow::Result<BTreeMap<String, Option<FileStamp>>> {
    Ok(dirty_repo_files(repo)?
        .into_iter()
        .map(|file| (file.path, file.stamp))
        .collect())
}

fn snapshot_dirty_file_map(repo: &IndexedRepoState) -> BTreeMap<String, Option<FileStamp>> {
    repo.dirty_files
        .iter()
        .map(|file| (file.path.clone(), file.stamp))
        .collect()
}

fn changed_files_between_heads(
    repo: &Repository,
    old_head: &str,
    new_head: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let old_oid = Oid::from_str(old_head)?;
    let new_oid = Oid::from_str(new_head)?;
    let old_tree = repo.find_commit(old_oid)?.tree()?;
    let new_tree = repo.find_commit(new_oid)?.tree()?;
    let mut opts = DiffOptions::new();
    let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(&mut opts))?;
    let mut paths = BTreeSet::new();
    diff.foreach(
        &mut |delta, _| {
            if let Some(path) = delta.new_file().path() {
                paths.insert(normalize_repo_path(path));
            }
            if let Some(path) = delta.old_file().path() {
                paths.insert(normalize_repo_path(path));
            }
            true
        },
        None,
        None,
        None,
    )?;
    Ok(paths)
}

fn compute_status(
    snapshot: IndexStatusSnapshot,
    project_root: &Path,
) -> anyhow::Result<IndexStatus> {
    let mut changed_files = BTreeSet::new();
    let mut freshness_tracking_available = false;
    let repo_status = match snapshot.repo.as_ref() {
        Some(indexed_repo) => match Repository::discover(project_root) {
            Ok(repo) => {
                freshness_tracking_available = true;
                let (current_head_oid, current_head_ref) = head_state(&repo);
                if let (Some(indexed_head), Some(current_head)) = (
                    indexed_repo.head_oid.as_deref(),
                    current_head_oid.as_deref(),
                ) {
                    if indexed_head != current_head {
                        match changed_files_between_heads(&repo, indexed_head, current_head) {
                            Ok(paths) => changed_files.extend(paths),
                            Err(_) => {
                                changed_files.insert(".git/HEAD".to_string());
                            }
                        }
                    }
                } else if indexed_repo.head_oid != current_head_oid {
                    changed_files.insert(".git/HEAD".to_string());
                }

                let indexed_dirty = snapshot_dirty_file_map(indexed_repo);
                let current_dirty = current_dirty_file_map(&repo)?;
                for path in indexed_dirty.keys().chain(current_dirty.keys()) {
                    let indexed_stamp = indexed_dirty.get(path);
                    let current_stamp = current_dirty.get(path);
                    if indexed_stamp != current_stamp {
                        changed_files.insert(path.clone());
                    }
                }

                Some(RepoStatus {
                    root: indexed_repo.root.clone(),
                    indexed_head_oid: indexed_repo.head_oid.clone(),
                    current_head_oid,
                    indexed_head_ref: indexed_repo.head_ref.clone(),
                    current_head_ref,
                    changed_file_count_since_index: changed_files.len(),
                    changed_files_since_index: changed_files.iter().cloned().collect(),
                })
            }
            Err(_) => Some(RepoStatus {
                root: indexed_repo.root.clone(),
                indexed_head_oid: indexed_repo.head_oid.clone(),
                current_head_oid: None,
                indexed_head_ref: indexed_repo.head_ref.clone(),
                current_head_ref: None,
                changed_file_count_since_index: 0,
                changed_files_since_index: Vec::new(),
            }),
        },
        None => None,
    };

    let borrowed_from = snapshot
        .borrowed_from
        .as_ref()
        .map(|source| BorrowedIndexStatus {
            project_root: source.project_root.clone(),
            store_dir: source.store_dir.clone(),
            migrated_at_unix_secs: source.migrated_at_unix_secs,
        });
    let temporary = borrowed_from.is_some();
    let note = if let Some(source) = borrowed_from.as_ref() {
        Some(format!(
            "temporary index migrated from {}; run `grapha index` to replace it with this worktree's index",
            source.project_root
        ))
    } else if !freshness_tracking_available && snapshot.repo.is_some() {
        Some("git status unavailable for this project root".to_string())
    } else {
        None
    };

    let changed_input_files = collect_changed_input_files(&changed_files);

    Ok(IndexStatus {
        indexed_at_unix_secs: snapshot.indexed_at_unix_secs,
        grapha_version: snapshot.grapha_version,
        node_count: snapshot.node_count,
        edge_count: snapshot.edge_count,
        temporary,
        may_be_stale: temporary
            || (freshness_tracking_available && !changed_input_files.is_empty()),
        freshness_tracking_available,
        changed_file_count_since_index: changed_files.len(),
        changed_input_file_count_since_index: changed_input_files.len(),
        changed_input_files_since_index: changed_input_files,
        repo: repo_status,
        borrowed_from,
        note,
    })
}

pub fn save_index_status(
    project_root: &Path,
    store_dir: &Path,
    node_count: usize,
    edge_count: usize,
    config: &GraphaConfig,
) -> anyhow::Result<()> {
    let (index_store_path, index_store_stamp) = current_index_store_info(project_root, config);
    let snapshot = IndexStatusSnapshot {
        version: INDEX_STATUS_VERSION,
        indexed_at_unix_secs: current_unix_secs(),
        grapha_version: env!("CARGO_PKG_VERSION").to_string(),
        node_count,
        edge_count,
        binary_stamp: cache::current_binary_stamp(),
        config_fingerprint: config.index_input_fingerprint(),
        index_store_path,
        index_store_stamp,
        repo: capture_repo_state(project_root)?,
        borrowed_from: None,
    };
    save_snapshot(store_dir, &snapshot)
}

pub fn save_borrowed_index_status(
    store_dir: &Path,
    source_project_root: &Path,
    source_store_dir: &Path,
    node_count: usize,
    edge_count: usize,
) -> anyhow::Result<()> {
    let source_snapshot = load_snapshot(source_store_dir).ok();
    let indexed_at_unix_secs = source_snapshot
        .as_ref()
        .map(|snapshot| snapshot.indexed_at_unix_secs)
        .unwrap_or_else(current_unix_secs);
    let grapha_version = source_snapshot
        .as_ref()
        .map(|snapshot| snapshot.grapha_version.clone())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let config_fingerprint = source_snapshot
        .as_ref()
        .map(|snapshot| snapshot.config_fingerprint.clone())
        .unwrap_or_default();
    let index_store_path = source_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.index_store_path.clone());
    let index_store_stamp = source_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.index_store_stamp);
    let repo = match source_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.repo.clone())
    {
        Some(repo) => Some(repo),
        None => capture_repo_state(source_project_root)?,
    };

    let snapshot = IndexStatusSnapshot {
        version: INDEX_STATUS_VERSION,
        indexed_at_unix_secs,
        grapha_version,
        node_count,
        edge_count,
        binary_stamp: cache::current_binary_stamp(),
        config_fingerprint,
        index_store_path,
        index_store_stamp,
        repo,
        borrowed_from: Some(BorrowedIndexSource {
            project_root: normalize_repo_path(source_project_root),
            store_dir: normalize_repo_path(source_store_dir),
            migrated_at_unix_secs: current_unix_secs(),
        }),
    };
    save_snapshot(store_dir, &snapshot)
}

pub fn store_has_borrowed_index(store_dir: &Path) -> bool {
    load_snapshot(store_dir)
        .ok()
        .and_then(|snapshot| snapshot.borrowed_from)
        .is_some()
}

pub fn plan_index_work(
    project_root: &Path,
    store_dir: &Path,
    config: &GraphaConfig,
) -> anyhow::Result<Option<IndexWorkPlan>> {
    if !config.external.is_empty() || !required_index_artifacts_exist(store_dir) {
        return Ok(None);
    }

    let snapshot = match load_snapshot(store_dir) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if status_path(store_dir).exists() {
                return Err(error);
            }
            return Ok(None);
        }
    };

    if snapshot.borrowed_from.is_some() {
        return Ok(None);
    }

    let status = compute_status(snapshot.clone(), project_root)?;
    if !status.freshness_tracking_available {
        return Ok(None);
    }

    let has_current_metadata =
        snapshot.binary_stamp.is_some() && !snapshot.config_fingerprint.is_empty();
    let compatible = if has_current_metadata {
        let Some(current_binary_stamp) = cache::current_binary_stamp() else {
            return Ok(None);
        };
        snapshot.binary_stamp == Some(current_binary_stamp)
            && snapshot.config_fingerprint == config.index_input_fingerprint()
            && snapshot_index_store_compatible(&snapshot, project_root, config)
    } else {
        legacy_snapshot_compatible(&snapshot, project_root, store_dir, config)
    };

    if !compatible {
        return Ok(None);
    }

    let mut rebuild_graph = false;
    let mut rebuild_localization = false;
    let mut rebuild_assets = false;
    for path in &status.changed_input_files_since_index {
        let kinds = classify_index_input(path);
        rebuild_graph |= kinds.graph;
        rebuild_localization |= kinds.localization;
        rebuild_assets |= kinds.assets;
    }

    Ok(Some(IndexWorkPlan {
        status,
        rebuild_graph,
        rebuild_localization,
        rebuild_assets,
    }))
}

pub fn load_index_status(project_root: &Path, store_dir: &Path) -> anyhow::Result<IndexStatus> {
    match load_snapshot(store_dir) {
        Ok(snapshot) => compute_status(snapshot, project_root),
        Err(error) => {
            if status_path(store_dir).exists() {
                Err(error)
            } else {
                legacy_status(store_dir)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphaConfig;
    use git2::{IndexAddOption, Signature};
    use std::time::Duration;
    use tempfile::tempdir;

    fn commit_all(repo: &Repository, message: &str) -> anyhow::Result<()> {
        let mut index = repo.index()?;
        index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = Signature::now("grapha", "grapha@example.com")?;
        let parent = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        if let Some(parent) = parent {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;
        } else {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?;
        }
        Ok(())
    }

    fn seed_index_artifacts(store_dir: &Path) {
        fs::create_dir_all(store_dir.join("search_index")).unwrap();
        fs::write(store_dir.join("grapha.db"), "").unwrap();
        fs::write(
            store_dir.join("localization.json"),
            r#"{"version":"1","records":[]}"#,
        )
        .unwrap();
        fs::write(
            store_dir.join("assets.json"),
            r#"{"version":"1","records":[]}"#,
        )
        .unwrap();
    }

    #[test]
    fn status_reports_clean_repo_as_fresh() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        save_index_status(dir.path(), &store_dir, 1, 0, &GraphaConfig::default()).unwrap();

        let status = load_index_status(dir.path(), &store_dir).unwrap();
        assert!(status.freshness_tracking_available);
        assert!(!status.may_be_stale);
        assert_eq!(status.changed_file_count_since_index, 0);
        assert_eq!(status.changed_input_file_count_since_index, 0);
    }

    #[test]
    fn status_detects_dirty_file_changes_since_index() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let source = dir.path().join("src.rs");
        fs::write(&source, "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        save_index_status(dir.path(), &store_dir, 1, 0, &GraphaConfig::default()).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        fs::write(&source, "fn main() { println!(\"hi\"); }\n").unwrap();

        let status = load_index_status(dir.path(), &store_dir).unwrap();
        assert!(status.may_be_stale);
        assert_eq!(status.changed_file_count_since_index, 1);
        assert_eq!(status.changed_input_file_count_since_index, 1);
        assert!(
            status
                .repo
                .unwrap()
                .changed_files_since_index
                .contains(&"src.rs".to_string())
        );
    }

    #[test]
    fn status_keeps_same_dirty_snapshot_fresh_until_file_changes_again() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let source = dir.path().join("src.rs");
        fs::write(&source, "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        fs::write(&source, "fn main() { println!(\"indexed\"); }\n").unwrap();
        let store_dir = dir.path().join(".grapha");
        save_index_status(dir.path(), &store_dir, 1, 0, &GraphaConfig::default()).unwrap();

        let status = load_index_status(dir.path(), &store_dir).unwrap();
        assert!(!status.may_be_stale);

        std::thread::sleep(Duration::from_millis(10));
        fs::write(&source, "fn main() { println!(\"changed\"); }\n").unwrap();
        let stale = load_index_status(dir.path(), &store_dir).unwrap();
        assert!(stale.may_be_stale);
        assert_eq!(stale.changed_file_count_since_index, 1);
        assert_eq!(stale.changed_input_file_count_since_index, 1);
    }

    #[test]
    fn plan_index_work_skips_when_repo_and_inputs_match() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        seed_index_artifacts(&store_dir);
        let config = GraphaConfig::default();
        save_index_status(dir.path(), &store_dir, 1, 0, &config).unwrap();

        let plan = plan_index_work(dir.path(), &store_dir, &config)
            .unwrap()
            .unwrap();
        assert!(
            plan.is_noop(),
            "matching inputs should allow a fast-path skip"
        );
    }

    #[test]
    fn plan_index_work_rejects_config_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        seed_index_artifacts(&store_dir);
        let indexed_config = GraphaConfig::default();
        save_index_status(dir.path(), &store_dir, 1, 0, &indexed_config).unwrap();

        let changed_config: GraphaConfig = toml::from_str(
            r#"
[[classifiers]]
pattern = "URLSession"
terminal = "network"
direction = "read"
operation = "HTTP"
        "#,
        )
        .unwrap();

        let status = plan_index_work(dir.path(), &store_dir, &changed_config).unwrap();
        assert!(
            status.is_none(),
            "config changes must invalidate the fast path"
        );
    }

    #[test]
    fn plan_index_work_requires_complete_artifacts() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();
        let config = GraphaConfig::default();
        save_index_status(dir.path(), &store_dir, 1, 0, &config).unwrap();

        let status = plan_index_work(dir.path(), &store_dir, &config).unwrap();
        assert!(
            status.is_none(),
            "missing artifacts should fall back to full indexing"
        );
    }

    #[test]
    fn plan_index_work_is_disabled_for_external_repos() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        seed_index_artifacts(&store_dir);
        let config: GraphaConfig = toml::from_str(
            r#"
[[external]]
name = "Shared"
path = "/tmp/shared"
"#,
        )
        .unwrap();
        save_index_status(dir.path(), &store_dir, 1, 0, &config).unwrap();

        let status = plan_index_work(dir.path(), &store_dir, &config).unwrap();
        assert!(
            status.is_none(),
            "externals keep the fast path conservative"
        );
    }

    #[test]
    fn status_ignores_docs_only_changes_for_staleness() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        save_index_status(dir.path(), &store_dir, 1, 0, &GraphaConfig::default()).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        fs::write(dir.path().join("README.md"), "updated\n").unwrap();

        let status = load_index_status(dir.path(), &store_dir).unwrap();
        assert!(!status.may_be_stale);
        assert_eq!(status.changed_file_count_since_index, 1);
        assert_eq!(status.changed_input_file_count_since_index, 0);
    }

    #[test]
    fn plan_index_work_rebuilds_only_localization_for_catalog_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        fs::write(
            dir.path().join("Localizable.xcstrings"),
            r#"{"sourceLanguage":"en","strings":{}}"#,
        )
        .unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        seed_index_artifacts(&store_dir);
        let config = GraphaConfig::default();
        save_index_status(dir.path(), &store_dir, 1, 0, &config).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        fs::write(
            dir.path().join("Localizable.xcstrings"),
            r#"{"sourceLanguage":"en","strings":{"hello":{"localizations":{"en":{"stringUnit":{"state":"translated","value":"Hello"}}}}}}"#,
        )
        .unwrap();

        let plan = plan_index_work(dir.path(), &store_dir, &config)
            .unwrap()
            .unwrap();
        assert!(!plan.is_noop());
        assert!(!plan.rebuild_graph);
        assert!(plan.rebuild_localization);
        assert!(!plan.rebuild_assets);
    }
}
