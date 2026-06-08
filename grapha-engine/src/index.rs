use std::collections::HashMap;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use grapha_core::graph::Graph;
use serde::Serialize;
use tantivy::Index;

use crate::assets::AssetCatalogIndex;
use crate::concepts::{self, ConceptIndex, ConceptSearchResult};
use crate::data_paths;
use crate::index_status::{self, IndexStatus};
use crate::localization::LocalizationCatalogIndex;
use crate::query;
use crate::search::{self, SearchOptions, SearchResult};
use crate::store::sqlite::SqliteStore;

#[derive(Debug, Clone, Serialize)]
pub struct CodebaseMetadata {
    pub id: String,
    pub name: String,
    pub project_root: PathBuf,
    pub store_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<IndexStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_error: Option<String>,
}

impl CodebaseMetadata {
    pub fn is_indexed(&self) -> bool {
        self.store_dir.join("grapha.db").is_file() && self.store_dir.join("search_index").is_dir()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Serialize)]
#[serde(tag = "direction", content = "result", rename_all = "snake_case")]
pub enum CodeTraceResult {
    Forward(query::trace::TraceResult),
    Reverse(query::reverse::ReverseResult),
}

pub struct GraphaSearchIndexHandle {
    project_root: PathBuf,
    store_dir: PathBuf,
    search_index: Index,
}

impl GraphaSearchIndexHandle {
    pub fn open_read_only(project_root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let project_root = project_root.as_ref();
        Self::open_store_read_only(project_root, project_root.join(".grapha"))
    }

    pub fn open_store_read_only(
        project_root: impl AsRef<Path>,
        store_dir: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        let project_root = project_root.as_ref().to_path_buf();
        let store_dir = store_dir.as_ref().to_path_buf();
        let search_index_path = store_dir.join("search_index");
        let search_index = Index::open_in_dir(&search_index_path)
            .with_context(|| format!("opening {} read-only", search_index_path.display()))?;
        Ok(Self {
            project_root,
            store_dir,
            search_index,
        })
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    pub fn metadata(&self) -> CodebaseMetadata {
        codebase_metadata_for_store(&self.project_root, &self.store_dir)
    }

    pub fn search_symbols(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        self.search_symbols_with_options(query, limit, &SearchOptions::default())
    }

    pub fn search_symbols_with_options(
        &self,
        query: &str,
        limit: usize,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>> {
        search::search_filtered(&self.search_index, query, limit, options)
    }
}

pub struct GraphaIndexHandle {
    project_root: PathBuf,
    store_dir: PathBuf,
    graph: Graph,
    search_index: Index,
    concept_index: ConceptIndex,
    localization_index: LocalizationCatalogIndex,
    asset_index: AssetCatalogIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphaIndexPoolConfig {
    pub max_cached_graph_bytes: usize,
    pub max_cached_handles: usize,
}

impl Default for GraphaIndexPoolConfig {
    fn default() -> Self {
        Self {
            max_cached_graph_bytes: 512 * 1024 * 1024,
            max_cached_handles: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphaIndexPoolStats {
    pub cached_handles: usize,
    pub cached_graph_bytes: usize,
    pub activations: usize,
    pub evictions: usize,
    pub max_cached_graph_bytes: usize,
    pub max_cached_handles: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StoreKey {
    project_root: PathBuf,
    store_dir: PathBuf,
}

impl StoreKey {
    fn new(project_root: impl AsRef<Path>, store_dir: impl AsRef<Path>) -> Self {
        Self {
            project_root: normalize_key_path(project_root.as_ref()),
            store_dir: normalize_key_path(store_dir.as_ref()),
        }
    }
}

struct PoolEntry {
    handle: Arc<GraphaIndexHandle>,
    graph_bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct GraphaIndexPoolState {
    entries: HashMap<StoreKey, PoolEntry>,
    clock: u64,
    activations: usize,
    evictions: usize,
}

pub struct GraphaIndexPool {
    config: GraphaIndexPoolConfig,
    state: Mutex<GraphaIndexPoolState>,
}

impl Default for GraphaIndexPool {
    fn default() -> Self {
        Self::new(GraphaIndexPoolConfig::default())
    }
}

impl GraphaIndexPool {
    pub fn new(config: GraphaIndexPoolConfig) -> Self {
        Self {
            config: GraphaIndexPoolConfig {
                max_cached_graph_bytes: config.max_cached_graph_bytes.max(1),
                max_cached_handles: config.max_cached_handles.max(1),
            },
            state: Mutex::new(GraphaIndexPoolState::default()),
        }
    }

    pub fn open(&self, project_root: impl AsRef<Path>) -> anyhow::Result<Arc<GraphaIndexHandle>> {
        let project_root = project_root.as_ref();
        self.open_store(project_root, project_root.join(".grapha"))
    }

    pub fn open_store(
        &self,
        project_root: impl AsRef<Path>,
        store_dir: impl AsRef<Path>,
    ) -> anyhow::Result<Arc<GraphaIndexHandle>> {
        let key = StoreKey::new(project_root, store_dir);
        if let Some(handle) = self.cached_handle(&key) {
            return Ok(handle);
        }

        let handle = Arc::new(GraphaIndexHandle::open_store_read_only(
            &key.project_root,
            &key.store_dir,
        )?);
        let graph_bytes = handle.estimated_graph_bytes();

        let mut state = self.state.lock().expect("index pool mutex poisoned");
        if let Some(entry) = state.entries.get(&key) {
            let existing = Arc::clone(&entry.handle);
            state.touch(&key);
            return Ok(existing);
        }

        state.clock += 1;
        let last_used = state.clock;
        state.activations += 1;
        state.entries.insert(
            key.clone(),
            PoolEntry {
                handle: Arc::clone(&handle),
                graph_bytes,
                last_used,
            },
        );
        state.evict_to_budget(&self.config, &key);
        Ok(handle)
    }

    pub fn stats(&self) -> GraphaIndexPoolStats {
        let state = self.state.lock().expect("index pool mutex poisoned");
        GraphaIndexPoolStats {
            cached_handles: state.entries.len(),
            cached_graph_bytes: state.cached_graph_bytes(),
            activations: state.activations,
            evictions: state.evictions,
            max_cached_graph_bytes: self.config.max_cached_graph_bytes,
            max_cached_handles: self.config.max_cached_handles,
        }
    }

    pub fn clear(&self) {
        let mut state = self.state.lock().expect("index pool mutex poisoned");
        state.entries.clear();
    }

    fn cached_handle(&self, key: &StoreKey) -> Option<Arc<GraphaIndexHandle>> {
        let mut state = self.state.lock().expect("index pool mutex poisoned");
        let handle = state
            .entries
            .get(key)
            .map(|entry| Arc::clone(&entry.handle));
        if handle.is_some() {
            state.touch(key);
        }
        handle
    }
}

impl GraphaIndexPoolState {
    fn cached_graph_bytes(&self) -> usize {
        self.entries.values().map(|entry| entry.graph_bytes).sum()
    }

    fn touch(&mut self, key: &StoreKey) {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_used = self.clock;
        }
    }

    fn evict_to_budget(&mut self, config: &GraphaIndexPoolConfig, protected_key: &StoreKey) {
        while self.entries.len() > config.max_cached_handles
            || (self.cached_graph_bytes() > config.max_cached_graph_bytes && self.entries.len() > 1)
        {
            let Some(victim_key) = self
                .entries
                .iter()
                .filter(|(key, _)| *key != protected_key || self.entries.len() == 1)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };

            if self.entries.remove(&victim_key).is_some() {
                self.evictions += 1;
            } else {
                break;
            }
        }
    }
}

impl GraphaIndexHandle {
    pub fn open_read_only(project_root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let project_root = project_root.as_ref();
        Self::open_store_read_only(project_root, project_root.join(".grapha"))
    }

    pub fn open_store_read_only(
        project_root: impl AsRef<Path>,
        store_dir: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        let project_root = project_root.as_ref().to_path_buf();
        let store_dir = store_dir.as_ref().to_path_buf();
        let graph_path = store_dir.join("grapha.db");
        let search_index_path = store_dir.join("search_index");

        let graph = SqliteStore::new(graph_path.clone())
            .load_read_only()
            .with_context(|| format!("opening {} read-only", graph_path.display()))?;
        let search_index = Index::open_in_dir(&search_index_path)
            .with_context(|| format!("opening {} read-only", search_index_path.display()))?;
        let concept_index = concepts::load_concept_index_from_store(&store_dir).unwrap_or_default();
        let localization_index =
            crate::localization::load_catalog_index_from_store(&store_dir).unwrap_or_default();
        let asset_index =
            crate::assets::load_asset_index_from_store(&store_dir).unwrap_or_default();

        Ok(Self {
            project_root,
            store_dir,
            graph,
            search_index,
            concept_index,
            localization_index,
            asset_index,
        })
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn estimated_graph_bytes(&self) -> usize {
        estimate_graph_bytes(&self.graph)
    }

    pub fn metadata(&self) -> CodebaseMetadata {
        codebase_metadata_for_store(&self.project_root, &self.store_dir)
    }

    pub fn search_symbols(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        self.search_symbols_with_options(query, limit, &SearchOptions::default())
    }

    pub fn search_symbols_with_options(
        &self,
        query: &str,
        limit: usize,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>> {
        search::search_filtered(&self.search_index, query, limit, options)
    }

    pub fn symbol_context(
        &self,
        symbol: &str,
    ) -> Result<query::ContextResult, query::QueryResolveError> {
        query::context::query_context(&self.graph, symbol)
    }

    pub fn impact(
        &self,
        symbol: &str,
        max_depth: usize,
    ) -> Result<query::impact::ImpactResult, query::QueryResolveError> {
        query::impact::query_impact_with_options(
            &self.graph,
            symbol,
            max_depth,
            &query::impact::ImpactQueryOptions::default(),
        )
    }

    pub fn trace(
        &self,
        symbol: &str,
        direction: TraceDirection,
        max_depth: usize,
    ) -> Result<CodeTraceResult, query::QueryResolveError> {
        match direction {
            TraceDirection::Forward => query::trace::query_trace(&self.graph, symbol, max_depth)
                .map(CodeTraceResult::Forward),
            TraceDirection::Reverse => query::reverse::query_reverse(&self.graph, symbol, None)
                .map(CodeTraceResult::Reverse),
        }
    }

    pub fn dataflow(
        &self,
        symbol: &str,
        max_depth: usize,
    ) -> Result<query::dataflow::DataflowResult, query::QueryResolveError> {
        query::dataflow::query_dataflow_with_options(
            &self.graph,
            symbol,
            max_depth,
            &query::dataflow::DataflowQueryOptions::default(),
        )
    }

    pub fn usages(
        &self,
        symbol: &str,
    ) -> Result<query::symbol_usages::SymbolUsagesResult, query::QueryResolveError> {
        query::symbol_usages::query_symbol_usages(
            &self.graph,
            symbol,
            query::symbol_usages::SymbolUsagesOptions::default(),
        )
    }

    pub fn dependencies(&self, name: Option<&str>) -> query::deps::DependencyReport {
        query::deps::query_dependencies(
            &self.graph,
            &query::deps::DependencyQueryOptions {
                name: name.map(str::to_string),
            },
        )
    }

    pub fn lookup_concept(&self, term: &str, limit: usize) -> anyhow::Result<ConceptSearchResult> {
        concepts::search_concepts_with_annotations(
            &self.graph,
            &self.search_index,
            &self.concept_index,
            &self.localization_index,
            &self.asset_index,
            term,
            limit,
            None,
        )
    }
}

fn normalize_key_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn estimate_graph_bytes(graph: &Graph) -> usize {
    mem::size_of::<Graph>()
        + graph.version.len()
        + graph.nodes.iter().map(estimate_node_bytes).sum::<usize>()
        + graph.edges.iter().map(estimate_edge_bytes).sum::<usize>()
}

fn estimate_node_bytes(node: &grapha_core::graph::Node) -> usize {
    mem::size_of::<grapha_core::graph::Node>()
        + node.id.len()
        + node.name.len()
        + path_bytes(&node.file)
        + node
            .metadata
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
        + node.signature.as_ref().map_or(0, String::len)
        + node.doc_comment.as_ref().map_or(0, String::len)
        + node.module.as_ref().map_or(0, String::len)
        + node.snippet.as_ref().map_or(0, String::len)
        + node.repo.as_ref().map_or(0, String::len)
}

fn estimate_edge_bytes(edge: &grapha_core::graph::Edge) -> usize {
    mem::size_of::<grapha_core::graph::Edge>()
        + edge.source.len()
        + edge.target.len()
        + edge.operation.as_ref().map_or(0, String::len)
        + edge.condition.as_ref().map_or(0, String::len)
        + edge.repo.as_ref().map_or(0, String::len)
        + edge
            .provenance
            .iter()
            .map(|provenance| {
                mem::size_of::<grapha_core::graph::EdgeProvenance>()
                    + path_bytes(&provenance.file)
                    + provenance.symbol_id.len()
            })
            .sum::<usize>()
}

fn path_bytes(path: &Path) -> usize {
    path.to_string_lossy().len()
}

pub fn codebase_metadata(project_root: impl AsRef<Path>) -> CodebaseMetadata {
    let project_root = project_root.as_ref();
    codebase_metadata_for_store(project_root, &project_root.join(".grapha"))
}

pub fn codebase_metadata_for_store(project_root: &Path, store_dir: &Path) -> CodebaseMetadata {
    let identity = data_paths::project_identity(project_root);
    let status_result = index_status::load_index_status(project_root, store_dir);
    let (status, status_error) = match status_result {
        Ok(status) => (Some(status), None),
        Err(error) => (None, Some(error.to_string())),
    };

    CodebaseMetadata {
        id: identity.project_id,
        name: data_paths::repo_name_for_project_root(project_root),
        project_root: project_root.to_path_buf(),
        store_dir: store_dir.to_path_buf(),
        status,
        status_error,
    }
}

pub fn list_codebases(root: impl AsRef<Path>) -> anyhow::Result<Vec<CodebaseMetadata>> {
    let root = root.as_ref();
    let mut project_roots = Vec::new();
    if root.join(".grapha").is_dir() {
        project_roots.push(root.to_path_buf());
    }

    for entry in fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.join(".grapha").is_dir() {
            project_roots.push(path);
        }
    }

    project_roots.sort();
    project_roots.dedup();

    Ok(project_roots.iter().map(codebase_metadata).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search;
    use crate::store::Store;
    use grapha_core::graph::{Node, NodeKind, Span, Visibility};
    use std::collections::HashMap;

    fn write_indexed_store(project_root: &Path, name: &str, payload_bytes: usize) -> usize {
        let store_dir = project_root.join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![Node {
                id: format!("src/main.rs::{name}"),
                kind: NodeKind::Function,
                name: name.to_string(),
                file: "src/main.rs".into(),
                span: Span {
                    start: [1, 0],
                    end: [1, 4],
                },
                visibility: Visibility::Public,
                metadata: HashMap::from([("test.payload".to_string(), "x".repeat(payload_bytes))]),
                role: None,
                signature: None,
                doc_comment: None,
                module: Some(name.to_string()),
                snippet: None,
                repo: Some(name.to_string()),
            }],
            edges: Vec::new(),
        };
        let estimated = estimate_graph_bytes(&graph);
        crate::store::sqlite::SqliteStore::new(store_dir.join("grapha.db"))
            .save(&graph)
            .unwrap();
        search::build_index(&graph, &store_dir.join("search_index")).unwrap();
        estimated
    }

    #[test]
    fn index_pool_evicts_lru_handles_under_handle_budget() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        write_indexed_store(&first, "first", 128);
        write_indexed_store(&second, "second", 128);

        let pool = GraphaIndexPool::new(GraphaIndexPoolConfig {
            max_cached_graph_bytes: usize::MAX,
            max_cached_handles: 1,
        });

        let first_handle = pool.open(&first).unwrap();
        assert_eq!(pool.stats().cached_handles, 1);

        let second_handle = pool.open(&second).unwrap();
        let stats = pool.stats();
        assert_eq!(stats.cached_handles, 1);
        assert_eq!(stats.activations, 2);
        assert_eq!(stats.evictions, 1);

        let reopened_first = pool.open(&first).unwrap();
        let stats = pool.stats();
        assert_eq!(stats.cached_handles, 1);
        assert_eq!(stats.activations, 3);
        assert_eq!(stats.evictions, 2);
        assert_eq!(first_handle.project_root(), first.canonicalize().unwrap());
        assert_eq!(second_handle.project_root(), second.canonicalize().unwrap());
        assert_eq!(reopened_first.project_root(), first.canonicalize().unwrap());
    }

    #[test]
    fn index_pool_evicts_to_cached_graph_byte_budget() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let first_estimate = write_indexed_store(&first, "first", 4096);
        let second_estimate = write_indexed_store(&second, "second", 4096);
        let budget = first_estimate.max(second_estimate) + 1;

        let pool = GraphaIndexPool::new(GraphaIndexPoolConfig {
            max_cached_graph_bytes: budget,
            max_cached_handles: 8,
        });

        let _first_handle = pool.open(&first).unwrap();
        assert!(pool.stats().cached_graph_bytes <= budget);
        let _second_handle = pool.open(&second).unwrap();
        let stats = pool.stats();
        assert_eq!(stats.cached_handles, 1);
        assert_eq!(stats.evictions, 1);
        assert!(
            stats.cached_graph_bytes <= budget,
            "cached graph bytes {} should stay within budget {budget}",
            stats.cached_graph_bytes
        );
    }

    #[test]
    fn search_index_handle_opens_tantivy_without_graph_handle() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(&project).unwrap();
        write_indexed_store(&project, "lookup_target", 128);

        let handle = GraphaSearchIndexHandle::open_read_only(&project).unwrap();
        let results = handle.search_symbols("lookup_target", 5).unwrap();

        assert_eq!(handle.project_root(), project.as_path());
        assert!(
            results.iter().any(|result| result.name == "lookup_target"),
            "search-only handle should return indexed symbols: {results:#?}"
        );
    }
}
