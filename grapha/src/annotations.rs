use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use grapha_core::graph::Node;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::data_paths::ProjectIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolAnnotationRecord {
    pub project_id: String,
    pub branch: String,
    pub repo: String,
    pub symbol_key: String,
    pub text: String,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub symbol_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolAnnotationView {
    pub project_id: String,
    pub branch: String,
    pub symbol_key: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AnnotationIndex {
    identity: AnnotationIdentity,
    records: HashMap<AnnotationRecordKey, SymbolAnnotationRecord>,
    records_by_project_symbol: HashMap<(String, String), Vec<AnnotationRecordKey>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AnnotationIdentity {
    project_id: String,
    branch: String,
}

impl From<ProjectIdentity> for AnnotationIdentity {
    fn from(identity: ProjectIdentity) -> Self {
        Self {
            project_id: identity.project_id,
            branch: identity.branch,
        }
    }
}

type AnnotationRecordKey = (String, String, String, String);

impl AnnotationIndex {
    pub fn get_for_node(&self, node: &Node) -> Option<SymbolAnnotationView> {
        self.record_for_node(node)
            .map(|record| record.view_for_node(node))
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn from_records(
        identity: AnnotationIdentity,
        records: HashMap<AnnotationRecordKey, SymbolAnnotationRecord>,
    ) -> Self {
        let mut records_by_project_symbol: HashMap<(String, String), Vec<AnnotationRecordKey>> =
            HashMap::new();
        for (project_id, branch, repo, symbol_key) in records.keys() {
            records_by_project_symbol
                .entry((project_id.clone(), symbol_key.clone()))
                .or_default()
                .push((
                    project_id.clone(),
                    branch.clone(),
                    repo.clone(),
                    symbol_key.clone(),
                ));
        }
        Self {
            identity,
            records,
            records_by_project_symbol,
        }
    }

    fn record_for_node(&self, node: &Node) -> Option<&SymbolAnnotationRecord> {
        let project_id = self.identity.project_id.as_str();
        let branch = self.identity.branch.as_str();
        let repo = repo_key(node);
        let symbol_key = symbol_key(node);

        let exact_key = record_key(project_id, branch, repo, symbol_key);
        if let Some(record) = self.records.get(&exact_key) {
            return Some(record);
        }

        let legacy_key = record_key(project_id, "", repo, symbol_key);
        if let Some(record) = self.records.get(&legacy_key) {
            return Some(record);
        }

        let global_legacy_key = record_key("", "", repo, symbol_key);
        if let Some(record) = self.records.get(&global_legacy_key) {
            return Some(record);
        }

        let matches = self
            .records_by_project_symbol
            .get(&(project_id.to_string(), symbol_key.to_string()))?;
        if matches.len() == 1 {
            self.records.get(&matches[0])
        } else {
            let matches = self
                .records_by_project_symbol
                .get(&(String::new(), symbol_key.to_string()))?;
            if matches.len() == 1 {
                self.records.get(&matches[0])
            } else {
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnnotationStore {
    path: PathBuf,
    import_from: Option<PathBuf>,
    identity: AnnotationIdentity,
}

impl AnnotationStore {
    #[allow(dead_code)]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            import_from: None,
            identity: AnnotationIdentity::default(),
        }
    }

    pub fn for_project_root(project_root: &Path) -> Self {
        Self::for_project_root_importing(
            project_root,
            project_root.join(".grapha").join("annotations.db"),
        )
    }

    pub(crate) fn for_project_root_importing(project_root: &Path, import_from: PathBuf) -> Self {
        Self {
            path: crate::data_paths::annotation_db_path(project_root),
            import_from: Some(import_from),
            identity: crate::data_paths::project_identity(project_root).into(),
        }
    }

    #[allow(dead_code)]
    pub fn for_project_root_with_data_root(project_root: &Path, data_root: &Path) -> Self {
        Self {
            path: crate::data_paths::annotation_db_path_with_data_root(project_root, data_root),
            import_from: Some(project_root.join(".grapha").join("annotations.db")),
            identity: crate::data_paths::project_identity(project_root).into(),
        }
    }

    pub fn for_project_id_with_data_root(
        project_id: &str,
        data_root: &Path,
    ) -> anyhow::Result<Self> {
        let project_id = validate_project_id(project_id)?;
        Ok(Self {
            path: data_root
                .join("repos")
                .join(project_id)
                .join("annotations.db"),
            import_from: None,
            identity: AnnotationIdentity {
                project_id: project_id.to_string(),
                branch: String::new(),
            },
        })
    }

    #[allow(dead_code)]
    pub fn for_store_dir(store_dir: &Path) -> Self {
        Self::new(store_dir.join("annotations.db"))
    }

    pub fn upsert_for_node(
        &self,
        node: &Node,
        text: &str,
        created_by: Option<&str>,
    ) -> anyhow::Result<SymbolAnnotationView> {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("annotation text cannot be empty");
        }
        let conn = self.open()?;
        create_tables(&conn)?;
        self.import_local_annotations(&conn)?;

        let existing_record = read_record_for_node(&conn, &self.identity, node)?;
        let storage_repo = existing_record
            .as_ref()
            .map(|record| record.repo.as_str())
            .unwrap_or_else(|| repo_key(node));
        let storage_project_id = existing_record
            .as_ref()
            .map(|record| record.project_id.as_str())
            .unwrap_or(self.identity.project_id.as_str());
        let storage_branch = existing_record
            .as_ref()
            .filter(|record| record.branch == self.identity.branch)
            .map(|record| record.branch.as_str())
            .unwrap_or(self.identity.branch.as_str());
        let key = symbol_key(node);
        let now = current_timestamp();
        let created_at = existing_record
            .as_ref()
            .map(|record| record.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let fingerprint = symbol_fingerprint(node);

        conn.execute(
            "INSERT INTO symbol_annotations (
                project_id, branch, repo, symbol_key, annotation, created_by,
                created_at, updated_at, symbol_fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(project_id, branch, repo, symbol_key) DO UPDATE SET
                annotation = excluded.annotation,
                created_by = COALESCE(excluded.created_by, symbol_annotations.created_by),
                updated_at = excluded.updated_at,
                symbol_fingerprint = excluded.symbol_fingerprint",
            params![
                storage_project_id,
                storage_branch,
                storage_repo,
                key,
                text,
                created_by,
                created_at,
                now,
                fingerprint,
            ],
        )?;

        let record = read_record(&conn, storage_project_id, storage_branch, storage_repo, key)?
            .ok_or_else(|| anyhow::anyhow!("annotation was not saved for symbol key {key}"))?;
        Ok(record.view_for_node(node))
    }

    pub fn get_for_node(&self, node: &Node) -> anyhow::Result<Option<SymbolAnnotationView>> {
        let Some(conn) = self.open_existing_or_import()? else {
            return Ok(None);
        };
        let record = read_record_for_node(&conn, &self.identity, node)?;
        Ok(record.map(|record| record.view_for_node(node)))
    }

    pub fn load_index(&self) -> anyhow::Result<AnnotationIndex> {
        let Some(conn) = self.open_existing_or_import()? else {
            return Ok(AnnotationIndex::default());
        };
        let mut stmt = conn.prepare(
            "SELECT project_id, branch, repo, symbol_key, annotation, created_by,
                    created_at, updated_at, symbol_fingerprint
             FROM symbol_annotations",
        )?;
        let mut rows = stmt.query([])?;
        let mut records = HashMap::new();
        while let Some(row) = rows.next()? {
            let record = SymbolAnnotationRecord {
                project_id: row.get(0)?,
                branch: row.get(1)?,
                repo: row.get(2)?,
                symbol_key: row.get(3)?,
                text: row.get(4)?,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                symbol_fingerprint: row.get(8)?,
            };
            records.insert(record.key(), record);
        }
        Ok(AnnotationIndex::from_records(
            self.identity.clone(),
            records,
        ))
    }

    pub fn list_records(&self) -> anyhow::Result<Vec<SymbolAnnotationRecord>> {
        let Some(conn) = self.open_existing_or_import()? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT project_id, branch, repo, symbol_key, annotation, created_by,
                    created_at, updated_at, symbol_fingerprint
             FROM symbol_annotations
             WHERE project_id = ?1 OR project_id = ''
             ORDER BY updated_at DESC, symbol_key ASC",
        )?;
        let mut rows = stmt.query(params![self.identity.project_id.as_str()])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            records.push(SymbolAnnotationRecord {
                project_id: row.get(0)?,
                branch: row.get(1)?,
                repo: row.get(2)?,
                symbol_key: row.get(3)?,
                text: row.get(4)?,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                symbol_fingerprint: row.get(8)?,
            });
        }
        Ok(records)
    }

    pub fn merge_records(&self, records: &[SymbolAnnotationRecord]) -> anyhow::Result<usize> {
        if records.is_empty() {
            return Ok(0);
        }

        let conn = self.open()?;
        create_tables(&conn)?;
        self.import_local_annotations(&conn)?;

        let mut merged = 0usize;
        for record in records {
            let text = record.text.trim();
            if record.symbol_key.trim().is_empty() {
                anyhow::bail!("synced annotation is missing symbol_key");
            }
            if text.is_empty() {
                anyhow::bail!("synced annotation text cannot be empty");
            }

            let project_id = if record.project_id.is_empty() {
                self.identity.project_id.as_str()
            } else {
                record.project_id.as_str()
            };
            let branch = record.branch.as_str();
            let repo = record.repo.as_str();
            let symbol_key = record.symbol_key.as_str();

            if let Some(existing) = read_record(&conn, project_id, branch, repo, symbol_key)?
                && timestamp_rank(&existing.updated_at) > timestamp_rank(&record.updated_at)
            {
                continue;
            }

            conn.execute(
                "INSERT INTO symbol_annotations (
                    project_id, branch, repo, symbol_key, annotation, created_by,
                    created_at, updated_at, symbol_fingerprint
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(project_id, branch, repo, symbol_key) DO UPDATE SET
                    annotation = excluded.annotation,
                    created_by = excluded.created_by,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at,
                    symbol_fingerprint = excluded.symbol_fingerprint",
                params![
                    project_id,
                    branch,
                    repo,
                    symbol_key,
                    text,
                    record.created_by.as_deref(),
                    record.created_at.as_str(),
                    record.updated_at.as_str(),
                    record.symbol_fingerprint.as_deref(),
                ],
            )?;
            merged += 1;
        }

        Ok(merged)
    }

    fn open(&self) -> anyhow::Result<Connection> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Connection::open(&self.path)?)
    }

    fn open_existing(&self) -> anyhow::Result<Option<Connection>> {
        if !self.path.exists() {
            return Ok(None);
        }
        Ok(Some(Connection::open(&self.path)?))
    }

    fn open_existing_or_import(&self) -> anyhow::Result<Option<Connection>> {
        let has_global = self.path.exists();
        let has_legacy = self
            .import_from
            .as_ref()
            .is_some_and(|source| source.exists() && !same_path(source, &self.path));

        let conn = if has_global {
            self.open_existing()?
        } else if has_legacy {
            Some(self.open()?)
        } else {
            None
        };

        if let Some(conn) = conn {
            create_tables(&conn)?;
            self.import_local_annotations(&conn)?;
            Ok(Some(conn))
        } else {
            Ok(None)
        }
    }

    fn import_local_annotations(&self, conn: &Connection) -> anyhow::Result<()> {
        let Some(source_path) = self.import_from.as_ref() else {
            return Ok(());
        };
        if !source_path.exists() || same_path(source_path, &self.path) {
            return Ok(());
        }

        create_tables(conn)?;
        let source_id = source_db_id(source_path);
        let already_imported = conn
            .query_row(
                "SELECT 1 FROM annotation_imports WHERE source_id = ?1",
                params![source_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if already_imported {
            return Ok(());
        }

        let legacy =
            Connection::open_with_flags(source_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !table_exists(&legacy, "symbol_annotations")? {
            mark_imported(conn, &source_id, source_path)?;
            return Ok(());
        }

        let project_expr = if table_has_column(&legacy, "symbol_annotations", "project_id")? {
            "project_id"
        } else {
            "''"
        };
        let branch_expr = if table_has_column(&legacy, "symbol_annotations", "branch")? {
            "branch"
        } else {
            "''"
        };
        let repo_expr = if table_has_column(&legacy, "symbol_annotations", "repo")? {
            "repo"
        } else {
            "''"
        };
        let query = format!(
            "SELECT {project_expr}, {branch_expr}, {repo_expr}, symbol_key, annotation, created_by,
                    created_at, updated_at, symbol_fingerprint
             FROM symbol_annotations"
        );
        let mut stmt = legacy.prepare(&query)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut project_id = row.get::<_, String>(0)?;
            if project_id.is_empty() {
                project_id = self.identity.project_id.clone();
            }
            conn.execute(
                "INSERT OR IGNORE INTO symbol_annotations (
                    project_id, branch, repo, symbol_key, annotation, created_by,
                    created_at, updated_at, symbol_fingerprint
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    project_id,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ],
            )?;
        }
        mark_imported(conn, &source_id, source_path)?;
        Ok(())
    }
}

impl SymbolAnnotationRecord {
    fn key(&self) -> AnnotationRecordKey {
        record_key(&self.project_id, &self.branch, &self.repo, &self.symbol_key)
    }

    fn view_for_node(&self, node: &Node) -> SymbolAnnotationView {
        let current_fingerprint = symbol_fingerprint(node);
        let stale = self
            .symbol_fingerprint
            .as_deref()
            .is_some_and(|stored| stored != current_fingerprint);
        SymbolAnnotationView {
            project_id: self.project_id.clone(),
            branch: self.branch.clone(),
            symbol_key: self.symbol_key.clone(),
            text: self.text.clone(),
            created_by: self.created_by.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            stale,
        }
    }
}

pub fn symbol_key(node: &Node) -> &str {
    node.id.as_str()
}

fn repo_key(node: &Node) -> &str {
    node.repo.as_deref().unwrap_or("")
}

fn record_key(project_id: &str, branch: &str, repo: &str, symbol_key: &str) -> AnnotationRecordKey {
    (
        project_id.to_string(),
        branch.to_string(),
        repo.to_string(),
        symbol_key.to_string(),
    )
}

fn validate_project_id(project_id: &str) -> anyhow::Result<&str> {
    let project_id = project_id.trim();
    if project_id.is_empty() {
        anyhow::bail!("project_id cannot be empty");
    }
    if !project_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("project_id contains unsupported characters");
    }
    Ok(project_id)
}

fn create_tables(conn: &Connection) -> anyhow::Result<()> {
    migrate_legacy_symbol_annotations(conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS symbol_annotations (
            project_id TEXT NOT NULL DEFAULT '',
            branch TEXT NOT NULL DEFAULT '',
            repo TEXT NOT NULL DEFAULT '',
            symbol_key TEXT NOT NULL,
            annotation TEXT NOT NULL,
            created_by TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            symbol_fingerprint TEXT,
            PRIMARY KEY (project_id, branch, repo, symbol_key)
        );
        CREATE INDEX IF NOT EXISTS idx_symbol_annotations_project_symbol
            ON symbol_annotations(project_id, symbol_key);
        CREATE TABLE IF NOT EXISTS annotation_imports (
            source_id TEXT PRIMARY KEY,
            source_path TEXT NOT NULL,
            imported_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}

fn migrate_legacy_symbol_annotations(conn: &Connection) -> anyhow::Result<()> {
    if !table_exists(conn, "symbol_annotations")?
        || table_has_column(conn, "symbol_annotations", "project_id")?
    {
        return Ok(());
    }

    conn.execute_batch(
        "DROP TABLE IF EXISTS symbol_annotations_legacy_migration;
         ALTER TABLE symbol_annotations RENAME TO symbol_annotations_legacy_migration;
         CREATE TABLE symbol_annotations (
            project_id TEXT NOT NULL DEFAULT '',
            branch TEXT NOT NULL DEFAULT '',
            repo TEXT NOT NULL DEFAULT '',
            symbol_key TEXT NOT NULL,
            annotation TEXT NOT NULL,
            created_by TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            symbol_fingerprint TEXT,
            PRIMARY KEY (project_id, branch, repo, symbol_key)
         );",
    )?;

    let repo_expr = if table_has_column(conn, "symbol_annotations_legacy_migration", "repo")? {
        "repo"
    } else {
        "''"
    };
    let copy = format!(
        "INSERT OR IGNORE INTO symbol_annotations (
            project_id, branch, repo, symbol_key, annotation, created_by,
            created_at, updated_at, symbol_fingerprint
         )
         SELECT '', '', {repo_expr}, symbol_key, annotation, created_by,
                created_at, updated_at, symbol_fingerprint
         FROM symbol_annotations_legacy_migration"
    );
    conn.execute(&copy, [])?;
    conn.execute_batch("DROP TABLE symbol_annotations_legacy_migration;")?;
    Ok(())
}

fn read_record_for_node(
    conn: &Connection,
    identity: &AnnotationIdentity,
    node: &Node,
) -> anyhow::Result<Option<SymbolAnnotationRecord>> {
    if let Some(record) = read_record(
        conn,
        &identity.project_id,
        &identity.branch,
        repo_key(node),
        symbol_key(node),
    )? {
        return Ok(Some(record));
    }

    if let Some(record) = read_record(
        conn,
        &identity.project_id,
        "",
        repo_key(node),
        symbol_key(node),
    )? {
        return Ok(Some(record));
    }

    if let Some(record) = read_record(conn, "", "", repo_key(node), symbol_key(node))? {
        return Ok(Some(record));
    }

    if let Some(record) =
        read_unique_record_for_project(conn, &identity.project_id, symbol_key(node))?
    {
        return Ok(Some(record));
    }

    if identity.project_id.is_empty() {
        return Ok(None);
    }

    read_unique_record_for_project(conn, "", symbol_key(node))
}

fn read_unique_record_for_project(
    conn: &Connection,
    project_id: &str,
    symbol_key: &str,
) -> anyhow::Result<Option<SymbolAnnotationRecord>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, branch, repo, symbol_key, annotation, created_by,
                created_at, updated_at, symbol_fingerprint
         FROM symbol_annotations
         WHERE project_id = ?1 AND symbol_key = ?2
         LIMIT 2",
    )?;
    let mut rows = stmt.query(params![project_id, symbol_key])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let record = SymbolAnnotationRecord {
        project_id: row.get(0)?,
        branch: row.get(1)?,
        repo: row.get(2)?,
        symbol_key: row.get(3)?,
        text: row.get(4)?,
        created_by: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        symbol_fingerprint: row.get(8)?,
    };
    if rows.next()?.is_some() {
        return Ok(None);
    }
    Ok(Some(record))
}

fn read_record(
    conn: &Connection,
    project_id: &str,
    branch: &str,
    repo: &str,
    symbol_key: &str,
) -> anyhow::Result<Option<SymbolAnnotationRecord>> {
    Ok(conn
        .query_row(
            "SELECT project_id, branch, repo, symbol_key, annotation, created_by,
                    created_at, updated_at, symbol_fingerprint
             FROM symbol_annotations
             WHERE project_id = ?1 AND branch = ?2 AND repo = ?3 AND symbol_key = ?4",
            params![project_id, branch, repo, symbol_key],
            |row| {
                Ok(SymbolAnnotationRecord {
                    project_id: row.get(0)?,
                    branch: row.get(1)?,
                    repo: row.get(2)?,
                    symbol_key: row.get(3)?,
                    text: row.get(4)?,
                    created_by: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    symbol_fingerprint: row.get(8)?,
                })
            },
        )
        .optional()?)
}

fn current_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    seconds.to_string()
}

fn timestamp_rank(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

fn mark_imported(conn: &Connection, source_id: &str, source_path: &Path) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO annotation_imports (source_id, source_path, imported_at)
         VALUES (?1, ?2, ?3)",
        params![
            source_id,
            source_path.to_string_lossy().as_ref(),
            current_timestamp()
        ],
    )?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
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

fn source_db_id(path: &Path) -> String {
    let normalized = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();
    let mut hasher = Fnv1a64::default();
    hasher.write_component(&normalized);
    format!("{:016x}", hasher.finish())
}

fn symbol_fingerprint(node: &Node) -> String {
    let mut hasher = Fnv1a64::default();
    hasher.write_component(&node.id);
    hasher.write_component(&format!("{:?}", node.kind));
    hasher.write_component(&node.name);
    hasher.write_component(&node.file.to_string_lossy());
    hasher.write_component(&format!("{:?}", node.span.start));
    hasher.write_component(&format!("{:?}", node.span.end));
    hasher.write_component(node.signature.as_deref().unwrap_or(""));
    hasher.write_component(node.doc_comment.as_deref().unwrap_or(""));
    hasher.write_component(node.snippet.as_deref().unwrap_or(""));
    format!("{:016x}", hasher.finish())
}

#[derive(Default)]
struct Fnv1a64(u64);

impl Fnv1a64 {
    fn write_component(&mut self, value: &str) {
        if self.0 == 0 {
            self.0 = 0xcbf29ce484222325;
        }
        for byte in value.as_bytes() {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x100000001b3);
    }

    fn finish(self) -> u64 {
        if self.0 == 0 {
            0xcbf29ce484222325
        } else {
            self.0
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use grapha_core::graph::{NodeKind, Span, Visibility};
    use rusqlite::Connection;

    use super::*;

    fn node() -> Node {
        Node {
            id: "s:DemoUSR".to_string(),
            kind: NodeKind::Function,
            name: "sendGift".to_string(),
            file: PathBuf::from("Sources/Gifts.swift"),
            span: Span {
                start: [1, 0],
                end: [5, 1],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: Some("func sendGift()".to_string()),
            doc_comment: None,
            module: Some("Demo".to_string()),
            snippet: Some("func sendGift() {}".to_string()),
            repo: None,
        }
    }

    fn store_for_branch(root: &Path, branch: &str) -> AnnotationStore {
        AnnotationStore {
            path: root.join("annotations.db"),
            import_from: None,
            identity: AnnotationIdentity {
                project_id: "demo-project".to_string(),
                branch: branch.to_string(),
            },
        }
    }

    #[test]
    fn store_for_project_id_uses_global_repo_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            AnnotationStore::for_project_id_with_data_root("remote-abc123", dir.path()).unwrap();

        assert_eq!(
            store.path,
            dir.path()
                .join("repos")
                .join("remote-abc123")
                .join("annotations.db")
        );
        assert_eq!(store.identity.project_id, "remote-abc123");
    }

    #[test]
    fn store_for_project_id_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let error =
            AnnotationStore::for_project_id_with_data_root("../other", dir.path()).unwrap_err();

        assert!(error.to_string().contains("unsupported characters"));
    }

    #[test]
    fn annotations_round_trip_by_symbol_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnnotationStore::for_store_dir(dir.path());
        let node = node();

        let saved = store
            .upsert_for_node(&node, "Coordinates the gift handoff.", Some("codex"))
            .unwrap();
        assert_eq!(saved.symbol_key, "s:DemoUSR");
        assert_eq!(saved.text, "Coordinates the gift handoff.");
        assert_eq!(saved.created_by.as_deref(), Some("codex"));
        assert!(!saved.stale);

        let loaded = store.get_for_node(&node).unwrap().unwrap();
        assert_eq!(loaded.text, saved.text);
        assert!(!loaded.stale);
    }

    #[test]
    fn annotation_view_marks_changed_symbol_stale() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnnotationStore::for_store_dir(dir.path());
        let mut node = node();
        store
            .upsert_for_node(&node, "Builds the checkout payload.", None)
            .unwrap();

        node.signature = Some("func sendGift(cartID: String)".to_string());
        let loaded = store.get_for_node(&node).unwrap().unwrap();
        assert!(loaded.stale);
    }

    #[test]
    fn branch_specific_annotation_overrides_shared_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let main_store = store_for_branch(dir.path(), "main");
        let feature_store = store_for_branch(dir.path(), "feature");
        let other_store = store_for_branch(dir.path(), "other");
        let node = node();

        main_store
            .upsert_for_node(&node, "Main branch explanation.", Some("codex"))
            .unwrap();
        let shared = feature_store.get_for_node(&node).unwrap().unwrap();
        assert_eq!(shared.text, "Main branch explanation.");
        assert_eq!(shared.branch, "main");

        feature_store
            .upsert_for_node(&node, "Feature branch explanation.", Some("codex"))
            .unwrap();

        let main = main_store.get_for_node(&node).unwrap().unwrap();
        let feature = feature_store.get_for_node(&node).unwrap().unwrap();
        assert_eq!(main.text, "Main branch explanation.");
        assert_eq!(feature.text, "Feature branch explanation.");
        assert_eq!(feature.branch, "feature");
        assert!(other_store.get_for_node(&node).unwrap().is_none());
    }

    #[test]
    fn merge_records_keeps_newer_annotation() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_for_branch(dir.path(), "main");
        let mut record = SymbolAnnotationRecord {
            project_id: "demo-project".to_string(),
            branch: "main".to_string(),
            repo: "".to_string(),
            symbol_key: "s:DemoUSR".to_string(),
            text: "Newer synced note.".to_string(),
            created_by: Some("remote".to_string()),
            created_at: "10".to_string(),
            updated_at: "20".to_string(),
            symbol_fingerprint: None,
        };

        assert_eq!(store.merge_records(&[record.clone()]).unwrap(), 1);
        record.text = "Older synced note.".to_string();
        record.updated_at = "19".to_string();
        assert_eq!(store.merge_records(&[record]).unwrap(), 0);

        let loaded = store.get_for_node(&node()).unwrap().unwrap();
        assert_eq!(loaded.text, "Newer synced note.");
        assert_eq!(loaded.branch, "main");
    }

    #[test]
    fn migrated_global_legacy_rows_remain_visible_to_project_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("annotations.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE symbol_annotations (
                repo TEXT NOT NULL DEFAULT '',
                symbol_key TEXT NOT NULL,
                annotation TEXT NOT NULL,
                created_by TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                symbol_fingerprint TEXT,
                PRIMARY KEY (repo, symbol_key)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbol_annotations (
                repo, symbol_key, annotation, created_by,
                created_at, updated_at, symbol_fingerprint
            ) VALUES ('', 's:DemoUSR', 'Legacy global note.', 'codex', '1', '1', NULL)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = store_for_branch(dir.path(), "main");
        let loaded = store.get_for_node(&node()).unwrap().unwrap();
        assert_eq!(loaded.text, "Legacy global note.");
        assert_eq!(loaded.project_id, "");
        assert_eq!(store.list_records().unwrap().len(), 1);
    }

    #[test]
    fn project_root_store_imports_legacy_annotations_without_mutating_source() {
        let project = tempfile::tempdir().unwrap();
        let global_root = tempfile::tempdir().unwrap();
        let legacy_store = AnnotationStore::for_store_dir(&project.path().join(".grapha"));
        let mut original_node = node();
        legacy_store
            .upsert_for_node(
                &original_node,
                "Legacy note from the local worktree.",
                Some("codex"),
            )
            .unwrap();

        original_node.signature = Some("func sendGift(cartID: String)".to_string());
        let global_store =
            AnnotationStore::for_project_root_with_data_root(project.path(), global_root.path());
        let imported = global_store
            .get_for_node(&original_node)
            .unwrap()
            .expect("legacy annotation should be imported on first read");

        assert_eq!(imported.text, "Legacy note from the local worktree.");
        assert!(imported.stale);
        assert!(
            project
                .path()
                .join(".grapha")
                .join("annotations.db")
                .exists()
        );
        assert!(legacy_store.get_for_node(&node()).unwrap().is_some());
    }

    #[test]
    fn global_annotation_wins_over_imported_legacy_conflict() {
        let project = tempfile::tempdir().unwrap();
        let global_root = tempfile::tempdir().unwrap();
        let global_store =
            AnnotationStore::for_project_root_with_data_root(project.path(), global_root.path());
        let legacy_store = AnnotationStore::for_store_dir(&project.path().join(".grapha"));
        let node = node();

        global_store
            .upsert_for_node(&node, "Global note should win.", Some("codex"))
            .unwrap();
        legacy_store
            .upsert_for_node(
                &node,
                "Legacy note should not replace it.",
                Some("old-agent"),
            )
            .unwrap();

        let loaded = global_store.get_for_node(&node).unwrap().unwrap();
        assert_eq!(loaded.text, "Global note should win.");
        assert_eq!(loaded.created_by.as_deref(), Some("codex"));
    }

    #[test]
    fn imported_sources_are_not_reimported_after_global_delete() {
        let project = tempfile::tempdir().unwrap();
        let global_root = tempfile::tempdir().unwrap();
        let legacy_store = AnnotationStore::for_store_dir(&project.path().join(".grapha"));
        let node = node();
        legacy_store
            .upsert_for_node(&node, "Import me once.", Some("codex"))
            .unwrap();

        let global_store =
            AnnotationStore::for_project_root_with_data_root(project.path(), global_root.path());
        assert!(global_store.get_for_node(&node).unwrap().is_some());

        let conn = Connection::open(&global_store.path).unwrap();
        conn.execute("DELETE FROM symbol_annotations", []).unwrap();
        drop(conn);

        assert!(global_store.get_for_node(&node).unwrap().is_none());
    }

    #[test]
    fn global_index_matches_unique_symbol_key_across_repo_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnnotationStore::for_store_dir(dir.path());
        let mut first_worktree_node = node();
        first_worktree_node.repo = Some("main-worktree".to_string());
        store
            .upsert_for_node(
                &first_worktree_node,
                "Shared across repo display names.",
                Some("codex"),
            )
            .unwrap();

        let mut second_worktree_node = first_worktree_node.clone();
        second_worktree_node.repo = Some("linked-worktree".to_string());
        let loaded = store.get_for_node(&second_worktree_node).unwrap().unwrap();

        assert_eq!(loaded.text, "Shared across repo display names.");
    }
}
