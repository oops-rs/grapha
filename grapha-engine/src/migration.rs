use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, bail};
use git2::Repository;
use rusqlite::{Connection, OpenFlags, params};

use crate::store::Store;
use crate::{annotations, cache, index_status, search, store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub source_project_root: PathBuf,
    pub source_store_dir: PathBuf,
    pub target_project_root: PathBuf,
    pub target_store_dir: PathBuf,
    pub node_count: usize,
    pub edge_count: usize,
    pub migrated_artifacts: Vec<String>,
    pub skipped_artifacts: Vec<String>,
    pub imported_annotations: bool,
}

#[derive(Debug, Clone)]
struct SourceStore {
    project_root: PathBuf,
    store_dir: PathBuf,
}

const DERIVED_JSON_ARTIFACTS: &[&str] = &["localization.json", "assets.json", "inferred.json"];
const USER_JSON_ARTIFACTS: &[&str] = &["concepts.json"];
const USER_SQLITE_ARTIFACTS: &[&str] = &["history.db"];

pub fn migrate_local_grapha(
    target_project_root: &Path,
    source: Option<&Path>,
    force: bool,
) -> anyhow::Result<MigrationReport> {
    let target_project_root = canonicalize_existing(target_project_root)
        .with_context(|| format!("resolving target {}", target_project_root.display()))?;
    let target_store_dir = target_project_root.join(".grapha");
    let source = match source {
        Some(source) => resolve_explicit_source(source)?,
        None => discover_source_store(&target_project_root)?,
    };

    if same_path(&source.store_dir, &target_store_dir) {
        bail!("source and target Grapha stores are the same directory");
    }

    let source_db = source.store_dir.join("grapha.db");
    if !source_db.is_file() {
        bail!(
            "source store {} has no grapha.db",
            source.store_dir.display()
        );
    }

    let target_was_borrowed = index_status::store_has_borrowed_index(&target_store_dir);
    let target_has_graph_artifacts = target_store_dir.join("grapha.db").exists()
        || target_store_dir.join("search_index").exists();
    if target_has_graph_artifacts && !force && !target_was_borrowed {
        bail!(
            "target already has a local Grapha index at {}; pass --force to replace it",
            target_store_dir.display()
        );
    }

    fs::create_dir_all(&target_store_dir)
        .with_context(|| format!("creating {}", target_store_dir.display()))?;

    let mut migrated_artifacts = Vec::new();
    let mut skipped_artifacts = Vec::new();

    backup_sqlite_db(&source_db, &target_store_dir.join("grapha.db"))
        .with_context(|| format!("copying {}", source_db.display()))?;
    migrated_artifacts.push("grapha.db".to_string());

    let graph = store::sqlite::SqliteStore::new(target_store_dir.join("grapha.db"))
        .load()
        .context("loading migrated graph")?;

    search::build_index(&graph, &target_store_dir.join("search_index"))
        .context("building search index for migrated graph")?;
    migrated_artifacts.push("search_index".to_string());

    for file_name in DERIVED_JSON_ARTIFACTS {
        sync_derived_file(
            &source.store_dir,
            &target_store_dir,
            file_name,
            &mut migrated_artifacts,
        )?;
    }

    for file_name in USER_JSON_ARTIFACTS {
        copy_user_file(
            &source.store_dir,
            &target_store_dir,
            file_name,
            force,
            &mut migrated_artifacts,
            &mut skipped_artifacts,
        )?;
    }

    for file_name in USER_SQLITE_ARTIFACTS {
        copy_user_sqlite(
            &source.store_dir,
            &target_store_dir,
            file_name,
            force,
            &mut migrated_artifacts,
            &mut skipped_artifacts,
        )?;
    }

    let annotation_db = source.store_dir.join("annotations.db");
    let imported_annotations = annotation_db.exists();
    if imported_annotations {
        annotations::AnnotationStore::for_project_root_importing(
            &source.project_root,
            annotation_db,
        )
        .load_index()
        .context("importing source annotations into the global annotation store")?;
        migrated_artifacts.push("annotations.db -> global annotations".to_string());
    }

    cache::GraphCache::new(&target_store_dir).invalidate();
    cache::QueryCache::new(&target_store_dir).invalidate();
    remove_file_if_exists(&target_store_dir.join("extraction_cache.bin"))?;

    index_status::save_borrowed_index_status(
        &target_store_dir,
        &source.project_root,
        &source.store_dir,
        graph.nodes.len(),
        graph.edges.len(),
    )?;
    migrated_artifacts.push("index_status.json".to_string());

    Ok(MigrationReport {
        source_project_root: source.project_root,
        source_store_dir: source.store_dir,
        target_project_root,
        target_store_dir,
        node_count: graph.nodes.len(),
        edge_count: graph.edges.len(),
        migrated_artifacts,
        skipped_artifacts,
        imported_annotations,
    })
}

fn resolve_explicit_source(source: &Path) -> anyhow::Result<SourceStore> {
    let source = canonicalize_existing(source)
        .with_context(|| format!("resolving source {}", source.display()))?;
    if source.join("grapha.db").is_file() {
        let project_root = if source.file_name().is_some_and(|name| name == ".grapha") {
            source
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| source.clone())
        } else {
            source.clone()
        };
        return Ok(SourceStore {
            project_root,
            store_dir: source,
        });
    }

    let store_dir = source.join(".grapha");
    if store_dir.join("grapha.db").is_file() {
        return Ok(SourceStore {
            project_root: source,
            store_dir,
        });
    }

    bail!(
        "source {} is neither a project root with .grapha/grapha.db nor a Grapha store directory",
        source.display()
    )
}

fn discover_source_store(target_project_root: &Path) -> anyhow::Result<SourceStore> {
    let repo = Repository::discover(target_project_root).with_context(|| {
        format!(
            "cannot auto-discover a source worktree from {}; pass --from <path>",
            target_project_root.display()
        )
    })?;
    let mut candidates = Vec::new();

    if let Some(main_root) = repo.commondir().parent() {
        candidates.push(main_root.to_path_buf());
    }

    if let Ok(names) = repo.worktrees() {
        for name in names.iter().flatten() {
            let Ok(worktree) = repo.find_worktree(name) else {
                continue;
            };
            if worktree.validate().is_ok() {
                candidates.push(worktree.path().to_path_buf());
            }
        }
    }

    let mut seen = BTreeSet::new();
    let mut usable = Vec::new();
    for candidate in candidates {
        let Ok(project_root) = canonicalize_existing(&candidate) else {
            continue;
        };
        if same_path(&project_root, target_project_root) {
            continue;
        }
        let key = project_root.to_string_lossy().to_string();
        if !seen.insert(key) {
            continue;
        }
        let store_dir = project_root.join(".grapha");
        let db_path = store_dir.join("grapha.db");
        if db_path.is_file() {
            usable.push(SourceStore {
                project_root,
                store_dir,
            });
        }
    }

    usable
        .into_iter()
        .max_by_key(|source| db_modified_at(&source.store_dir.join("grapha.db")))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no sibling worktree with .grapha/grapha.db was found; pass --from <path>"
            )
        })
}

fn backup_sqlite_db(source: &Path, target: &Path) -> anyhow::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp = target.with_file_name(format!(
        ".{}.migrating",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sqlite.db")
    ));
    remove_sqlite_files(&temp)?;

    let source_conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening source sqlite database {}", source.display()))?;
    source_conn
        .execute(
            "VACUUM main INTO ?1",
            params![temp.to_string_lossy().as_ref()],
        )
        .with_context(|| format!("backing up sqlite database to {}", temp.display()))?;

    remove_sqlite_files(target)?;
    fs::rename(&temp, target)
        .with_context(|| format!("moving {} to {}", temp.display(), target.display()))?;
    remove_sqlite_sidecars(&temp)?;
    Ok(())
}

fn sync_derived_file(
    source_store_dir: &Path,
    target_store_dir: &Path,
    file_name: &str,
    migrated_artifacts: &mut Vec<String>,
) -> anyhow::Result<()> {
    let source = source_store_dir.join(file_name);
    let target = target_store_dir.join(file_name);
    if source.exists() {
        copy_regular_file(&source, &target)?;
        migrated_artifacts.push(file_name.to_string());
    } else {
        remove_file_if_exists(&target)?;
    }
    Ok(())
}

fn copy_user_file(
    source_store_dir: &Path,
    target_store_dir: &Path,
    file_name: &str,
    force: bool,
    migrated_artifacts: &mut Vec<String>,
    skipped_artifacts: &mut Vec<String>,
) -> anyhow::Result<()> {
    let source = source_store_dir.join(file_name);
    let target = target_store_dir.join(file_name);
    if !source.exists() {
        return Ok(());
    }
    if target.exists() && !force {
        skipped_artifacts.push(format!("{file_name} (target exists)"));
        return Ok(());
    }
    copy_regular_file(&source, &target)?;
    migrated_artifacts.push(file_name.to_string());
    Ok(())
}

fn copy_user_sqlite(
    source_store_dir: &Path,
    target_store_dir: &Path,
    file_name: &str,
    force: bool,
    migrated_artifacts: &mut Vec<String>,
    skipped_artifacts: &mut Vec<String>,
) -> anyhow::Result<()> {
    let source = source_store_dir.join(file_name);
    let target = target_store_dir.join(file_name);
    if !source.exists() {
        return Ok(());
    }
    if target.exists() && !force {
        skipped_artifacts.push(format!("{file_name} (target exists)"));
        return Ok(());
    }
    backup_sqlite_db(&source, &target)?;
    migrated_artifacts.push(file_name.to_string());
    Ok(())
}

fn copy_regular_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, target)
        .with_context(|| format!("copying {} to {}", source.display(), target.display()))?;
    Ok(())
}

fn remove_sqlite_files(path: &Path) -> anyhow::Result<()> {
    remove_file_if_exists(path)?;
    remove_sqlite_sidecars(path)
}

fn remove_sqlite_sidecars(path: &Path) -> anyhow::Result<()> {
    remove_file_if_exists(&PathBuf::from(format!("{}-wal", path.to_string_lossy())))?;
    remove_file_if_exists(&PathBuf::from(format!("{}-shm", path.to_string_lossy())))?;
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

fn canonicalize_existing(path: &Path) -> anyhow::Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("canonicalizing {}", path.display()))
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn db_modified_at(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use grapha_core::graph::{
        Edge, EdgeKind, EdgeProvenance, Graph, Node, NodeKind, Span, Visibility,
    };
    use tempfile::tempdir;

    use super::*;

    fn sample_graph() -> Graph {
        Graph {
            version: env!("CARGO_PKG_VERSION").to_string(),
            nodes: vec![Node {
                id: "sample::main".to_string(),
                kind: NodeKind::Function,
                name: "main".to_string(),
                file: PathBuf::from("src/main.rs"),
                span: Span {
                    start: [0, 0],
                    end: [1, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: Some("fn main()".to_string()),
                doc_comment: None,
                module: Some("sample".to_string()),
                snippet: Some("fn main() {}".to_string()),
                repo: None,
            }],
            edges: vec![Edge {
                source: "sample::main".to_string(),
                target: "sample::main".to_string(),
                kind: EdgeKind::Uses,
                confidence: 1.0,
                provenance: vec![EdgeProvenance {
                    file: PathBuf::from("src/main.rs"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    symbol_id: "sample::main".to_string(),
                }],
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                repo: None,
            }],
        }
    }

    fn seed_store(project_root: &Path) {
        let store_dir = project_root.join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();
        let graph = sample_graph();
        store::sqlite::SqliteStore::new(store_dir.join("grapha.db"))
            .save(&graph)
            .unwrap();
        fs::write(store_dir.join("localization.json"), "{}").unwrap();
        fs::write(store_dir.join("assets.json"), "{}").unwrap();
    }

    #[test]
    fn explicit_migration_copies_graph_and_marks_target_temporary() {
        let source = tempdir().unwrap();
        let target = tempdir().unwrap();
        seed_store(source.path());
        fs::write(
            source.path().join(".grapha").join("concepts.json"),
            "{\"version\":1,\"concepts\":[]}",
        )
        .unwrap();

        let report = migrate_local_grapha(target.path(), Some(source.path()), false).unwrap();

        assert_eq!(report.node_count, 1);
        assert_eq!(report.edge_count, 1);
        assert!(target.path().join(".grapha/grapha.db").exists());
        assert!(target.path().join(".grapha/search_index").is_dir());
        assert!(target.path().join(".grapha/concepts.json").exists());

        let status =
            index_status::load_index_status(target.path(), &target.path().join(".grapha")).unwrap();
        assert!(status.temporary);
        assert!(status.may_be_stale);
        assert!(status.borrowed_from.is_some());
        assert!(
            index_status::plan_index_work(
                target.path(),
                &target.path().join(".grapha"),
                &crate::config::GraphaConfig::default()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn migration_preserves_existing_user_files_without_force() {
        let source = tempdir().unwrap();
        let target = tempdir().unwrap();
        seed_store(source.path());
        fs::write(source.path().join(".grapha/concepts.json"), "source").unwrap();
        fs::create_dir_all(target.path().join(".grapha")).unwrap();
        fs::write(target.path().join(".grapha/concepts.json"), "target").unwrap();

        let report = migrate_local_grapha(target.path(), Some(source.path()), false).unwrap();

        assert_eq!(
            fs::read_to_string(target.path().join(".grapha/concepts.json")).unwrap(),
            "target"
        );
        assert!(
            report
                .skipped_artifacts
                .iter()
                .any(|artifact| artifact.starts_with("concepts.json"))
        );
    }

    #[test]
    fn migration_auto_discovers_sibling_worktree_store() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        fs::create_dir(&main).unwrap();
        fs::write(main.join("lib.rs"), "pub struct Shared;\n").unwrap();

        run_git(&main, &["init"]);
        run_git(&main, &["add", "lib.rs"]);
        run_git(
            &main,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test User",
                "commit",
                "-m",
                "init",
            ],
        );
        run_git(&main, &["worktree", "add", linked.to_str().unwrap()]);
        seed_store(&main);

        let report = migrate_local_grapha(&linked, None, false).unwrap();

        assert_eq!(report.source_project_root, main.canonicalize().unwrap());
        assert!(linked.join(".grapha/grapha.db").exists());
        assert!(linked.join(".grapha/search_index").is_dir());
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
