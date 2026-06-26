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
    /// Repo workdir, stored relative to the index root so the persisted
    /// `index_status.json` is portable (a copied/moved `.grapha` carries no
    /// absolute host paths). Legacy snapshots stored an absolute path here;
    /// see [`resolve_repo_root`] for the read-time resolution that keeps both
    /// shapes working. When the repo root cannot be expressed relative to the
    /// index root (e.g. it lives outside it), this is `"."` and the live
    /// `project_root` supplies the absolute path at read time.
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

/// The index root that a `.grapha` store sits under (its parent directory).
/// Repo paths are persisted relative to this so the artifact is portable.
fn index_root_for_store(store_dir: &Path) -> PathBuf {
    store_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| store_dir.to_path_buf())
}

/// Express `repo_root` relative to the index root for portable storage.
///
/// Returns `"."` when the repo root is the index root, a forward-slashed
/// relative path when it is an ancestor of the index root (e.g. the index
/// lives in a subdirectory of the repo), and `"."` as a conservative fallback
/// when no relative expression is possible. The absolute path is never
/// persisted, so a copied/moved `.grapha` stays host-independent.
fn relativize_repo_root(repo_root: &Path, store_dir: &Path) -> String {
    let index_root = index_root_for_store(store_dir);
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let index_root = index_root.canonicalize().unwrap_or(index_root);

    if repo_root == index_root {
        return ".".to_string();
    }
    if let Ok(suffix) = index_root.strip_prefix(&repo_root) {
        // Index root is nested inside the repo: walk up from it.
        let ups = suffix.components().count();
        let mut rel = PathBuf::new();
        for _ in 0..ups {
            rel.push("..");
        }
        if rel.as_os_str().is_empty() {
            return ".".to_string();
        }
        return normalize_repo_path(&rel);
    }
    if let Ok(suffix) = repo_root.strip_prefix(&index_root) {
        // Repo root is nested inside the index root.
        if suffix.as_os_str().is_empty() {
            return ".".to_string();
        }
        return normalize_repo_path(suffix);
    }
    ".".to_string()
}

/// Resolve a persisted repo `root` back to an absolute path at read time.
///
/// Legacy snapshots stored an absolute path; new snapshots store a path
/// relative to the index root. Absolute stored values are returned as-is;
/// relative values are resolved against the live index root (derived from the
/// store dir via [`index_root_for_store`]), so a moved `.grapha` reports the
/// host where it is now opened rather than where it was produced.
///
/// This is the read-time inverse of [`relativize_repo_root`]: both pivot on the
/// *index root* (`store_dir.parent()`), so they stay symmetric even when the
/// store lives in a project subdirectory (`--store-dir`) and the index root is
/// therefore not the same as the caller's `project_root`.
fn resolve_repo_root(stored: &str, store_dir: &Path) -> String {
    let stored_path = Path::new(stored);
    if stored_path.is_absolute() {
        return stored.to_string();
    }
    let index_root = index_root_for_store(store_dir);
    let resolved = index_root.join(stored_path);
    let resolved = resolved.canonicalize().unwrap_or(resolved);
    normalize_repo_path(&resolved)
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
        .and_then(|head| head.shorthand().ok())
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
        let Ok(path) = entry.path() else {
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

fn capture_repo_state(
    project_root: &Path,
    store_dir: &Path,
) -> anyhow::Result<Option<IndexedRepoState>> {
    let repo = match Repository::discover(project_root) {
        Ok(repo) => repo,
        Err(_) => return Ok(None),
    };
    let Some(root) = repo_root(&repo) else {
        return Ok(None);
    };
    let (head_oid, head_ref) = head_state(&repo);
    Ok(Some(IndexedRepoState {
        root: relativize_repo_root(&root, store_dir),
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
    store_dir: &Path,
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
                    root: resolve_repo_root(&indexed_repo.root, store_dir),
                    indexed_head_oid: indexed_repo.head_oid.clone(),
                    current_head_oid,
                    indexed_head_ref: indexed_repo.head_ref.clone(),
                    current_head_ref,
                    changed_file_count_since_index: changed_files.len(),
                    changed_files_since_index: changed_files.iter().cloned().collect(),
                })
            }
            Err(_) => Some(RepoStatus {
                root: resolve_repo_root(&indexed_repo.root, store_dir),
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
        repo: capture_repo_state(project_root, store_dir)?,
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
        None => capture_repo_state(source_project_root, store_dir)?,
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

    let status = compute_status(snapshot.clone(), project_root, store_dir)?;
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
        Ok(snapshot) => compute_status(snapshot, project_root, store_dir),
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
    fn index_status_json_carries_no_absolute_repo_root() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let store_dir = dir.path().join(".grapha");
        save_index_status(dir.path(), &store_dir, 1, 0, &GraphaConfig::default()).unwrap();

        let raw = fs::read_to_string(status_path(&store_dir)).unwrap();
        let absolute = normalize_repo_path(&dir.path().canonicalize().unwrap());
        assert!(
            !raw.contains(&absolute),
            "portable index_status.json must not embed the absolute repo root: {raw}"
        );
        // The persisted repo root is relative to the index root.
        let snapshot = load_snapshot(&store_dir).unwrap();
        let stored = snapshot.repo.unwrap().root;
        assert!(
            !Path::new(&stored).is_absolute(),
            "stored repo root should be relative, got {stored}"
        );
    }

    #[test]
    fn copied_grapha_opens_read_only_after_move() {
        // Produce an index in one location, then copy the source tree + .grapha
        // to a different absolute path and confirm the status still resolves
        // against the new location (portability of the artifact).
        let origin = tempdir().unwrap();
        let repo = Repository::init(origin.path()).unwrap();
        fs::write(origin.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        let origin_store = origin.path().join(".grapha");
        save_index_status(origin.path(), &origin_store, 1, 0, &GraphaConfig::default()).unwrap();

        // Move: copy the whole tree (source + .grapha) to a new root.
        let moved = tempdir().unwrap();
        let moved_root = moved.path().join("relocated");
        copy_dir_all(origin.path(), &moved_root).unwrap();
        let moved_store = moved_root.join(".grapha");

        // The status loads and reports the *new* root, proving no stale
        // absolute path from the producing host leaked into the artifact.
        let status = load_index_status(&moved_root, &moved_store).unwrap();
        let reported_root = status.repo.expect("repo status present").root;
        let expected = normalize_repo_path(&moved_root.canonicalize().unwrap());
        assert_eq!(reported_root, expected);
        assert!(
            !reported_root.contains(&normalize_repo_path(origin.path())),
            "moved artifact must not report the producing host path"
        );
    }

    // Acceptance: docs/adr/0027 Decision 7 — "fix the one absolute-path leak
    // (repo.root in index_status.json)". The in-repo common case persists "."
    // so the artifact is host-independent.
    #[test]
    fn test_relativize_repo_root_in_repo_is_dot() {
        let dir = tempdir().unwrap();
        // store at <repo>/.grapha ⇒ index root == repo root ⇒ ".".
        let store_dir = dir.path().join(".grapha");
        let rel = relativize_repo_root(dir.path(), &store_dir);
        assert_eq!(
            rel, ".",
            "in-repo .grapha should persist '.' not an absolute path"
        );
        assert!(!Path::new(&rel).is_absolute());
    }

    // When .grapha is nested below the repo root, the stored value walks up with
    // `..` and still carries no absolute host path.
    #[test]
    fn test_relativize_repo_root_nested_index_walks_up() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("sub").join("dir");
        fs::create_dir_all(&nested).unwrap();
        let store_dir = nested.join(".grapha");
        let rel = relativize_repo_root(dir.path(), &store_dir);
        assert!(
            !Path::new(&rel).is_absolute(),
            "nested index root must stay relative: {rel}"
        );
        // Two levels deep ⇒ "../..".
        assert_eq!(rel, "../..");
    }

    // Acceptance: docs/adr/0027 Decision 7 — read-time resolution. A relative
    // stored value is rebuilt against the *live* project root, while a legacy
    // absolute value is returned untouched so old index_status.json keeps working.
    #[test]
    fn test_resolve_repo_root_relative_rebinds_to_live_root() {
        let dir = tempdir().unwrap();
        // resolve_repo_root takes the stored path relative to the live index
        // root, derived from the store dir's parent — the symmetric inverse of
        // relativize_repo_root, which pivots on the same index root.
        let store_dir = dir.path().join(".grapha");
        let resolved = resolve_repo_root(".", &store_dir);
        let expected = normalize_repo_path(&dir.path().canonicalize().unwrap());
        assert_eq!(
            resolved, expected,
            "relative root resolves against the index root (store_dir parent)"
        );
    }

    #[test]
    fn test_resolve_repo_root_absolute_is_returned_as_is() {
        // A legacy absolute stored value is host-specific but must not be
        // re-resolved against the live root (backward compatibility).
        let legacy = if cfg!(windows) {
            "C:/old/host/repo"
        } else {
            "/old/host/repo"
        };
        let live = tempdir().unwrap();
        let store_dir = live.path().join(".grapha");
        assert_eq!(resolve_repo_root(legacy, &store_dir), legacy);
    }

    // Regression: relativize_repo_root and resolve_repo_root must be symmetric
    // even when project_root != store_dir.parent() (the `--store-dir` case, where
    // the store is pointed into a project subdirectory). Both pivot on the index
    // root (store_dir.parent()); a value relativized at write time must resolve
    // back to the original repo root at read time, including after a move.
    #[test]
    fn test_relativize_resolve_symmetric_when_store_in_subdir() {
        let dir = tempdir().unwrap();
        let repo_root = dir.path().canonicalize().unwrap();
        // Store lives in a *subdirectory* of the project, so its index root
        // (build/index/) is NOT the project root.
        let index_root = repo_root.join("build").join("index");
        let store_dir = index_root.join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();

        // Write-time: express the repo root relative to the index root.
        let stored = relativize_repo_root(&repo_root, &store_dir);
        assert!(
            !Path::new(&stored).is_absolute(),
            "stored repo root must stay relative: {stored}"
        );
        // index root is repo/build/index ⇒ two levels up to the repo root.
        assert_eq!(stored, "../..");

        // Read-time: resolving with the SAME store dir recovers the repo root,
        // regardless of any `project_root` a caller might pass elsewhere.
        let resolved = resolve_repo_root(&stored, &store_dir);
        assert_eq!(
            resolved,
            normalize_repo_path(&repo_root),
            "relativize/resolve must round-trip through the index root"
        );
    }

    // End-to-end: a copied/moved store that lives in a project subdirectory
    // still resolves the repo root against the *new* host, proving the
    // write-time relativize and read-time resolve agree on the index root.
    #[test]
    fn copied_store_in_project_subdir_resolves_after_move() {
        let origin = tempdir().unwrap();
        let repo = Repository::init(origin.path()).unwrap();
        fs::write(origin.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        // Store under <repo>/build/index/.grapha — project_root (the repo) is
        // NOT store_dir.parent().
        let origin_store = origin.path().join("build").join("index").join(".grapha");
        fs::create_dir_all(&origin_store).unwrap();
        save_index_status(origin.path(), &origin_store, 1, 0, &GraphaConfig::default()).unwrap();

        let stored = load_snapshot(&origin_store).unwrap().repo.unwrap().root;
        assert!(
            !Path::new(&stored).is_absolute(),
            "subdir store root must be relative: {stored}"
        );

        // Copy the whole tree (source + nested store) to a new absolute root.
        let moved = tempdir().unwrap();
        let moved_root = moved.path().join("relocated");
        copy_dir_all(origin.path(), &moved_root).unwrap();
        let moved_store = moved_root.join("build").join("index").join(".grapha");

        // Read back: project_root is the moved repo root, store_dir is the
        // moved nested store. The repo root resolves to the NEW host.
        let status = load_index_status(&moved_root, &moved_store).unwrap();
        let reported = status.repo.expect("repo status present").root;
        assert!(
            !reported.contains(&normalize_repo_path(origin.path())),
            "moved subdir store must not report the producing host: {reported}"
        );
        assert_eq!(
            reported,
            normalize_repo_path(&moved_root.canonicalize().unwrap()),
            "subdir store resolves to the new repo root"
        );
    }

    // Acceptance: docs/adr/0027 Decision 7 — a copied/moved .grapha opens and
    // resolves against the new host even when the store lives in a subdirectory
    // of the source tree (nested index root), exercising the `..`-walk path.
    #[test]
    fn copied_grapha_with_nested_store_resolves_after_move() {
        let origin = tempdir().unwrap();
        let repo = Repository::init(origin.path()).unwrap();
        fs::write(origin.path().join("src.rs"), "fn main() {}\n").unwrap();
        commit_all(&repo, "initial").unwrap();

        // Nest the store under <repo>/index/.grapha.
        let origin_store = origin.path().join("index").join(".grapha");
        fs::create_dir_all(&origin_store).unwrap();
        save_index_status(origin.path(), &origin_store, 1, 0, &GraphaConfig::default()).unwrap();

        // The persisted root must be relative (walks up to the repo).
        let stored = load_snapshot(&origin_store).unwrap().repo.unwrap().root;
        assert!(
            !Path::new(&stored).is_absolute(),
            "nested store root must be relative: {stored}"
        );

        // Copy the whole tree to a new absolute location.
        let moved = tempdir().unwrap();
        let moved_root = moved.path().join("relocated");
        copy_dir_all(origin.path(), &moved_root).unwrap();
        let moved_index_root = moved_root.join("index");
        let moved_store = moved_index_root.join(".grapha");

        // Read back against the new index root; the relative `..` resolves to
        // the new host's repo root, not the producing host.
        let status = load_index_status(&moved_index_root, &moved_store).unwrap();
        let reported = status.repo.expect("repo status present").root;
        assert!(
            !reported.contains(&normalize_repo_path(origin.path())),
            "moved nested artifact must not report the producing host: {reported}"
        );
        let expected = normalize_repo_path(&moved_root.canonicalize().unwrap());
        assert_eq!(
            reported, expected,
            "nested artifact resolves to the new repo root"
        );
    }

    fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let target = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_all(&entry.path(), &target)?;
            } else {
                fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
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
