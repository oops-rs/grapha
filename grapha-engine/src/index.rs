use std::fs;
use std::path::{Path, PathBuf};

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

pub struct GraphaIndexHandle {
    project_root: PathBuf,
    store_dir: PathBuf,
    graph: Graph,
    search_index: Index,
    concept_index: ConceptIndex,
    localization_index: LocalizationCatalogIndex,
    asset_index: AssetCatalogIndex,
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
