use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use grapha_core::extract::ExtractionResult;
use grapha_core::graph::Graph;
use serde::{Deserialize, Serialize};

/// Returns `true` if `cache_path` exists and its modification time is
/// greater than or equal to that of `source_path`.
pub fn cache_is_fresh(source_path: &Path, cache_path: &Path) -> bool {
    let mtime = |p: &Path| -> Option<SystemTime> { fs::metadata(p).ok()?.modified().ok() };

    match (mtime(source_path), mtime(cache_path)) {
        (Some(src), Some(cache)) => cache >= src,
        _ => false,
    }
}

/// Binary (bincode) cache for a [`Graph`] stored alongside the SQLite database.
pub struct GraphCache {
    cache_path: PathBuf,
}

impl GraphCache {
    /// The cache file lives at `store_dir/graph.bincode`.
    pub fn new(store_dir: &Path) -> Self {
        Self {
            cache_path: store_dir.join("graph.bincode"),
        }
    }

    /// Returns `true` when the cache file is at least as new as `db_path`.
    pub fn is_fresh(&self, db_path: &Path) -> bool {
        cache_is_fresh(db_path, &self.cache_path)
    }

    /// Deserialise a [`Graph`] from the binary cache file.
    pub fn load(&self) -> anyhow::Result<Graph> {
        let bytes = fs::read(&self.cache_path)
            .with_context(|| format!("reading cache file {}", self.cache_path.display()))?;
        let graph: Graph = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map(|(graph, _)| graph)
            .with_context(|| format!("deserialising cache file {}", self.cache_path.display()))?;
        Ok(graph)
    }

    /// Serialise `graph` to the binary cache file, creating parent directories
    /// if they do not yet exist.
    pub fn save(&self, graph: &Graph) -> anyhow::Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serde::encode_to_vec(graph, bincode::config::standard())
            .with_context(|| "serialising graph to bincode".to_string())?;
        fs::write(&self.cache_path, bytes)
            .with_context(|| format!("writing cache file {}", self.cache_path.display()))?;
        Ok(())
    }
}

const QUERY_CACHE_FILENAME: &str = "query_cache.bin";
const MAX_QUERY_CACHE_ENTRIES: usize = 64;
const EXTRACTION_CACHE_FILENAME: &str = "extraction_cache.bin";
const EXTRACTION_CACHE_FORMAT_VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct QueryCacheEntry {
    db_mtime_secs: u64,
    output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub len: u64,
    pub modified_secs: u64,
    pub modified_nanos: u32,
}

impl FileStamp {
    pub fn from_path(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            len: metadata.len(),
            modified_secs: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionCacheEntry {
    pub stamp: FileStamp,
    pub module_name: Option<String>,
    #[serde(default)]
    pub config_fingerprint: String,
    pub result: ExtractionResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtractionCacheFile {
    format_version: u32,
    binary_stamp: Option<FileStamp>,
    entries: HashMap<String, ExtractionCacheEntry>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ExtractionCacheDiskFormat {
    Current(ExtractionCacheFile),
    Legacy(#[allow(dead_code)] HashMap<String, ExtractionCacheEntry>),
}

pub struct ExtractionCache {
    cache_path: PathBuf,
}

/// Cache for serialized query output, keyed by a string and invalidated when
/// the SQLite database changes.
pub struct QueryCache {
    cache_path: PathBuf,
}

impl ExtractionCache {
    pub fn new(store_dir: &Path) -> Self {
        Self {
            cache_path: store_dir.join(EXTRACTION_CACHE_FILENAME),
        }
    }

    pub fn load_entries(&self) -> anyhow::Result<HashMap<String, ExtractionCacheEntry>> {
        let Ok(contents) = fs::read_to_string(&self.cache_path) else {
            return Ok(HashMap::new());
        };
        let cache_file: ExtractionCacheDiskFormat =
            serde_json::from_str(&contents).with_context(|| {
                format!(
                    "deserialising extraction cache {}",
                    self.cache_path.display()
                )
            })?;

        let Some(current_binary_stamp) = current_binary_stamp() else {
            return Ok(HashMap::new());
        };

        Ok(match cache_file {
            ExtractionCacheDiskFormat::Current(cache)
                if cache.format_version == EXTRACTION_CACHE_FORMAT_VERSION
                    && cache.binary_stamp == Some(current_binary_stamp) =>
            {
                cache.entries
            }
            ExtractionCacheDiskFormat::Current(_) | ExtractionCacheDiskFormat::Legacy(_) => {
                HashMap::new()
            }
        })
    }

    pub fn save_entries(
        &self,
        entries: &HashMap<String, ExtractionCacheEntry>,
    ) -> anyhow::Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string(&ExtractionCacheFile {
            format_version: EXTRACTION_CACHE_FORMAT_VERSION,
            binary_stamp: current_binary_stamp(),
            entries: entries.clone(),
        })
        .with_context(|| format!("serialising extraction cache {}", self.cache_path.display()))?;
        fs::write(&self.cache_path, contents)
            .with_context(|| format!("writing extraction cache {}", self.cache_path.display()))?;
        Ok(())
    }
}

pub(crate) fn current_binary_stamp() -> Option<FileStamp> {
    let executable = std::env::current_exe().ok()?;
    FileStamp::from_path(&executable)
}

fn mtime_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

impl QueryCache {
    /// The cache file lives at `store_dir/query_cache.bin`.
    pub fn new(store_dir: &Path) -> Self {
        Self {
            cache_path: store_dir.join(QUERY_CACHE_FILENAME),
        }
    }

    fn load_entries(&self) -> HashMap<String, QueryCacheEntry> {
        let Ok(bytes) = fs::read(&self.cache_path) else {
            return HashMap::new();
        };
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map(|(entries, _)| entries)
            .unwrap_or_default()
    }

    /// Returns the cached output for `key` if the cache entry exists and the
    /// database file has not changed since it was written.
    pub fn get(&self, key: &str, db_path: &Path) -> Option<String> {
        let current_mtime = mtime_secs(db_path)?;
        let entries = self.load_entries();
        let entry = entries.get(key)?;
        if entry.db_mtime_secs == current_mtime {
            Some(entry.output.clone())
        } else {
            None
        }
    }

    /// Inserts or updates the cached output for `key`, evicting stale entries
    /// and enforcing the maximum cache size.
    pub fn put(&self, key: &str, db_path: &Path, output: &str) -> anyhow::Result<()> {
        let current_mtime = mtime_secs(db_path).unwrap_or(0);

        let mut entries = self.load_entries();

        // Evict entries that belong to a different db mtime (stale after index).
        entries.retain(|_, v| v.db_mtime_secs == current_mtime);

        // If the cache is full, clear it rather than implementing LRU.
        if entries.len() >= MAX_QUERY_CACHE_ENTRIES {
            entries.clear();
        }

        entries.insert(
            key.to_owned(),
            QueryCacheEntry {
                db_mtime_secs: current_mtime,
                output: output.to_owned(),
            },
        );

        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serde::encode_to_vec(&entries, bincode::config::standard())
            .context("serialising query cache to bincode")?;
        fs::write(&self.cache_path, bytes)
            .with_context(|| format!("writing query cache {}", self.cache_path.display()))?;
        Ok(())
    }

    /// Removes the cache file so the next query rebuilds it from scratch.
    pub fn invalidate(&self) {
        let _ = fs::remove_file(&self.cache_path);
    }
}

impl GraphCache {
    /// Removes the cache file so the next load rebuilds from SQLite.
    pub fn invalidate(&self) {
        let _ = fs::remove_file(&self.cache_path);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use grapha_core::graph::{Node, NodeKind, Span, Visibility};

    use super::*;

    fn sample_extraction_result(file: &str) -> ExtractionResult {
        let mut result = ExtractionResult::new();
        result.nodes.push(Node {
            id: format!("{file}::main"),
            kind: NodeKind::Function,
            name: "main".to_string(),
            file: PathBuf::from(file),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("sample".to_string()),
            snippet: Some("fn main() {}".to_string()),
            repo: None,
        });
        result
    }

    // ── cache_is_fresh ────────────────────────────────────────────────────────

    #[test]
    fn cache_is_stale_when_source_is_newer() {
        let dir = tempfile::tempdir().unwrap();

        let source = dir.path().join("source.db");
        let cache = dir.path().join("graph.bincode");

        // Write the cache first so its mtime is older.
        fs::write(&cache, b"old").unwrap();

        // Sleep long enough that the filesystem records a newer mtime for source.
        thread::sleep(Duration::from_millis(10));
        fs::write(&source, b"new source").unwrap();

        // Cache was written before source → stale.
        assert!(!cache_is_fresh(&source, &cache));
    }

    #[test]
    fn cache_is_stale_when_cache_missing() {
        let dir = tempfile::tempdir().unwrap();

        let source = dir.path().join("source.db");
        let cache = dir.path().join("graph.bincode");

        fs::write(&source, b"data").unwrap();
        // cache does not exist → not fresh.

        assert!(!cache_is_fresh(&source, &cache));
    }

    // ── GraphCache ────────────────────────────────────────────────────────────

    #[test]
    fn graph_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("grapha.db");
        fs::write(&db_path, b"fake db").unwrap();

        let gc = GraphCache::new(dir.path());

        let original = Graph::new();
        gc.save(&original).unwrap();

        // The cache file must exist and be at least as new as the db file.
        assert!(gc.cache_path.exists());
        assert!(gc.is_fresh(&db_path));

        let loaded = gc.load().unwrap();
        assert_eq!(loaded, original);
    }

    #[test]
    fn graph_cache_returns_none_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("grapha.db");
        fs::write(&db_path, b"db").unwrap();

        let gc = GraphCache::new(dir.path());

        // No cache file written → not fresh.
        assert!(!gc.is_fresh(&db_path));

        // load() should return an error because the file doesn't exist.
        assert!(gc.load().is_err());
    }

    // ── QueryCache ────────────────────────────────────────────────────────────

    #[test]
    fn query_cache_hit_returns_cached_output() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("grapha.db");
        fs::write(&db_path, b"db").unwrap();

        let qc = QueryCache::new(dir.path());

        qc.put("my_key", &db_path, "hello world").unwrap();

        let result = qc.get("my_key", &db_path);
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    #[test]
    fn query_cache_miss_when_db_changes() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("grapha.db");
        fs::write(&db_path, b"db v1").unwrap();

        let qc = QueryCache::new(dir.path());
        qc.put("key", &db_path, "cached output").unwrap();

        // Ensure enough time passes for the filesystem mtime to differ.
        thread::sleep(Duration::from_secs(1));
        fs::write(&db_path, b"db v2").unwrap();

        let result = qc.get("key", &db_path);
        assert!(
            result.is_none(),
            "cache should be invalidated after db write"
        );
    }

    #[test]
    fn query_cache_different_keys_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("grapha.db");
        fs::write(&db_path, b"db").unwrap();

        let qc = QueryCache::new(dir.path());

        qc.put("key_a", &db_path, "output_a").unwrap();
        qc.put("key_b", &db_path, "output_b").unwrap();

        assert_eq!(qc.get("key_a", &db_path).as_deref(), Some("output_a"));
        assert_eq!(qc.get("key_b", &db_path).as_deref(), Some("output_b"));
    }

    #[test]
    fn file_stamp_changes_when_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        let first = FileStamp::from_path(&file).unwrap();

        thread::sleep(Duration::from_millis(10));
        fs::write(&file, "fn main() { println!(\"hi\"); }\n").unwrap();
        let second = FileStamp::from_path(&file).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn extraction_cache_round_trips_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ExtractionCache::new(dir.path());
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut entries = HashMap::new();
        entries.insert(
            "main.rs".to_string(),
            ExtractionCacheEntry {
                stamp: FileStamp::from_path(&file).unwrap(),
                module_name: Some("sample".to_string()),
                config_fingerprint: "fingerprint:v1".to_string(),
                result: sample_extraction_result("main.rs"),
            },
        );

        cache.save_entries(&entries).unwrap();
        let loaded = cache.load_entries().unwrap();

        assert_eq!(loaded.len(), 1);
        let entry = loaded.get("main.rs").unwrap();
        assert_eq!(entry.module_name.as_deref(), Some("sample"));
        assert_eq!(entry.config_fingerprint, "fingerprint:v1");
        assert_eq!(entry.result.nodes[0].name, "main");
    }

    #[test]
    fn extraction_cache_ignores_legacy_disk_format() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ExtractionCache::new(dir.path());
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut legacy_entries = HashMap::new();
        legacy_entries.insert(
            "main.rs".to_string(),
            ExtractionCacheEntry {
                stamp: FileStamp::from_path(&file).unwrap(),
                module_name: Some("sample".to_string()),
                config_fingerprint: "legacy".to_string(),
                result: sample_extraction_result("main.rs"),
            },
        );

        fs::write(
            &cache.cache_path,
            serde_json::to_string(&legacy_entries).unwrap(),
        )
        .unwrap();

        let loaded = cache.load_entries().unwrap();
        assert!(loaded.is_empty(), "legacy caches should be invalidated");
    }

    #[test]
    fn extraction_cache_ignores_mismatched_binary_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ExtractionCache::new(dir.path());
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut entries = HashMap::new();
        entries.insert(
            "main.rs".to_string(),
            ExtractionCacheEntry {
                stamp: FileStamp::from_path(&file).unwrap(),
                module_name: Some("sample".to_string()),
                config_fingerprint: "fingerprint:v1".to_string(),
                result: sample_extraction_result("main.rs"),
            },
        );

        let current_stamp = current_binary_stamp().expect("current test binary should be readable");
        let mismatched_stamp = FileStamp {
            len: current_stamp.len.saturating_add(1),
            modified_secs: current_stamp.modified_secs,
            modified_nanos: current_stamp.modified_nanos,
        };

        fs::write(
            &cache.cache_path,
            serde_json::to_string(&ExtractionCacheFile {
                format_version: EXTRACTION_CACHE_FORMAT_VERSION,
                binary_stamp: Some(mismatched_stamp),
                entries,
            })
            .unwrap(),
        )
        .unwrap();

        let loaded = cache.load_entries().unwrap();
        assert!(
            loaded.is_empty(),
            "caches from another binary should be invalidated"
        );
    }
}
