use std::collections::BTreeMap;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEventKind {
    Commit,
    Build,
    Test,
    Deploy,
    Incident,
}

impl HistoryEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Build => "build",
            Self::Test => "test",
            Self::Deploy => "deploy",
            Self::Incident => "incident",
        }
    }

    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "commit" => Ok(Self::Commit),
            "build" => Ok(Self::Build),
            "test" => Ok(Self::Test),
            "deploy" => Ok(Self::Deploy),
            "incident" => Ok(Self::Incident),
            other => bail!("unknown history event kind: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub id: String,
    pub kind: HistoryEventKind,
    pub timestamp: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct NewHistoryEvent {
    pub kind: HistoryEventKind,
    pub timestamp: Option<String>,
    pub title: String,
    pub status: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub detail: Option<String>,
    pub files: Vec<String>,
    pub modules: Vec<String>,
    pub symbols: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryListFilter {
    pub kind: Option<HistoryEventKind>,
    pub file: Option<String>,
    pub module: Option<String>,
    pub symbol: Option<String>,
    pub limit: usize,
}

pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn for_project(project_root: &Path) -> Self {
        Self::new(project_root.join(".grapha/history.db"))
    }

    fn open(&self) -> anyhow::Result<Connection> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let conn = Connection::open(&self.path)?;
        create_tables(&conn)?;
        Ok(conn)
    }

    pub fn add(&self, event: NewHistoryEvent) -> anyhow::Result<HistoryEvent> {
        let conn = self.open()?;
        let event = event.into_event();
        conn.execute(
            "INSERT OR REPLACE INTO history_events (
                id, kind, timestamp, title, status, commit_sha, branch, detail,
                files_json, modules_json, symbols_json, metadata_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                event.id,
                event.kind.as_str(),
                event.timestamp,
                event.title,
                event.status,
                event.commit,
                event.branch,
                event.detail,
                serde_json::to_string(&event.files)?,
                serde_json::to_string(&event.modules)?,
                serde_json::to_string(&event.symbols)?,
                serde_json::to_string(&event.metadata)?,
            ],
        )?;
        Ok(event)
    }

    pub fn list(&self, filter: &HistoryListFilter) -> anyhow::Result<Vec<HistoryEvent>> {
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, timestamp, title, status, commit_sha, branch, detail,
                    files_json, modules_json, symbols_json, metadata_json
             FROM history_events
             ORDER BY timestamp DESC, id DESC",
        )?;
        let mut rows = stmt.query([])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let event = decode_event(row)?;
            if matches_filter(&event, filter) {
                events.push(event);
                if filter.limit > 0 && events.len() == filter.limit {
                    break;
                }
            }
        }
        Ok(events)
    }
}

impl NewHistoryEvent {
    fn into_event(self) -> HistoryEvent {
        let timestamp = self.timestamp.unwrap_or_else(default_timestamp);
        let id = history_event_id(self.kind, &timestamp, &self.title, &self.symbols);
        HistoryEvent {
            id,
            kind: self.kind,
            timestamp,
            title: self.title,
            status: self.status,
            commit: self.commit,
            branch: self.branch,
            detail: self.detail,
            files: self.files,
            modules: self.modules,
            symbols: self.symbols,
            metadata: self.metadata,
        }
    }
}

fn create_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS history_events (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL,
            timestamp     TEXT NOT NULL,
            title         TEXT NOT NULL,
            status        TEXT,
            commit_sha    TEXT,
            branch        TEXT,
            detail        TEXT,
            files_json    TEXT NOT NULL,
            modules_json  TEXT NOT NULL,
            symbols_json  TEXT NOT NULL,
            metadata_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_history_kind_time
            ON history_events(kind, timestamp DESC);",
    )?;
    Ok(())
}

fn decode_event(row: &rusqlite::Row<'_>) -> anyhow::Result<HistoryEvent> {
    let kind: String = row.get(1)?;
    Ok(HistoryEvent {
        id: row.get(0)?,
        kind: HistoryEventKind::parse(&kind)?,
        timestamp: row.get(2)?,
        title: row.get(3)?,
        status: row.get(4)?,
        commit: row.get(5)?,
        branch: row.get(6)?,
        detail: row.get(7)?,
        files: serde_json::from_str(&row.get::<_, String>(8)?)?,
        modules: serde_json::from_str(&row.get::<_, String>(9)?)?,
        symbols: serde_json::from_str(&row.get::<_, String>(10)?)?,
        metadata: serde_json::from_str(&row.get::<_, String>(11)?)?,
    })
}

fn matches_filter(event: &HistoryEvent, filter: &HistoryListFilter) -> bool {
    if let Some(kind) = filter.kind
        && event.kind != kind
    {
        return false;
    }
    if let Some(file) = filter.file.as_deref()
        && !event.files.iter().any(|candidate| candidate.contains(file))
    {
        return false;
    }
    if let Some(module) = filter.module.as_deref()
        && !event.modules.iter().any(|candidate| candidate == module)
    {
        return false;
    }
    if let Some(symbol) = filter.symbol.as_deref()
        && !event.symbols.iter().any(|candidate| candidate == symbol)
    {
        return false;
    }
    true
}

fn default_timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    millis.to_string()
}

fn history_event_id(
    kind: HistoryEventKind,
    timestamp: &str,
    title: &str,
    symbols: &[String],
) -> String {
    let mut hasher = Fnv1a64::default();
    hasher.write_component(kind.as_str());
    hasher.write_component(timestamp);
    hasher.write_component(title);
    for symbol in symbols {
        hasher.write_component(symbol);
    }
    format!(
        "{}-{}-{:016x}",
        sanitize_id_part(timestamp),
        kind.as_str(),
        hasher.finish()
    )
}

fn sanitize_id_part(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    sanitized.trim_matches('-').to_string()
}

#[derive(Default)]
struct Fnv1a64 {
    state: u64,
}

impl Fnv1a64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn write_component(&mut self, value: &str) {
        if self.state == 0 {
            self.state = Self::OFFSET_BASIS;
        }
        for byte in value.as_bytes() {
            self.write_u8(*byte);
        }
        self.write_u8(0xff);
    }
}

impl Hasher for Fnv1a64 {
    fn finish(&self) -> u64 {
        if self.state == 0 {
            Self::OFFSET_BASIS
        } else {
            self.state
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.state == 0 {
            self.state = Self::OFFSET_BASIS;
        }
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_store_round_trips_and_filters_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path().join("history.db"));
        let event = store
            .add(NewHistoryEvent {
                kind: HistoryEventKind::Test,
                timestamp: Some("2026-04-24T10:00:00Z".to_string()),
                title: "cargo test".to_string(),
                status: Some("passed".to_string()),
                commit: Some("abc123".to_string()),
                branch: Some("main".to_string()),
                detail: Some("workspace tests passed".to_string()),
                files: vec!["src/lib.rs".to_string()],
                modules: vec!["core".to_string()],
                symbols: vec!["src/lib.rs::run".to_string()],
                metadata: BTreeMap::from([("duration_ms".to_string(), "1200".to_string())]),
            })
            .unwrap();

        let listed = store
            .list(&HistoryListFilter {
                kind: Some(HistoryEventKind::Test),
                file: Some("lib.rs".to_string()),
                module: Some("core".to_string()),
                symbol: Some("src/lib.rs::run".to_string()),
                limit: 10,
            })
            .unwrap();

        assert_eq!(listed, vec![event]);
    }

    #[test]
    fn history_store_limit_zero_means_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path().join("history.db"));
        for index in 0..2 {
            store
                .add(NewHistoryEvent {
                    kind: HistoryEventKind::Build,
                    timestamp: Some(format!("2026-04-24T10:00:0{index}Z")),
                    title: format!("build {index}"),
                    status: None,
                    commit: None,
                    branch: None,
                    detail: None,
                    files: Vec::new(),
                    modules: Vec::new(),
                    symbols: Vec::new(),
                    metadata: BTreeMap::new(),
                })
                .unwrap();
        }

        let listed = store
            .list(&HistoryListFilter {
                limit: 0,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(listed.len(), 2);
    }
}
