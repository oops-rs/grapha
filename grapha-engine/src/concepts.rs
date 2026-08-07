use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tantivy::Index;

use grapha_core::graph::{Edge, EdgeKind, Graph, Node, NodeKind};

use crate::annotations::AnnotationIndex;
use crate::assets::{self, AssetCatalogIndex, AssetRecord};
use crate::fields::FieldSet;
use crate::localization::{LocalizationCatalogIndex, LocalizationCatalogRecord};
use crate::query::{self, SymbolInfo};
use crate::search::{self, SearchOptions};
use crate::snippet::compact_symbol_snippet;
use crate::symbol_locator::SymbolLocatorIndex;

const CONCEPTS_SNAPSHOT_VERSION: &str = "1";
const CONCEPTS_SNAPSHOT_FILE: &str = "concepts.json";
pub const DEFAULT_CONCEPT_SEARCH_LIMIT: usize = 20;

const STATUS_CONFIRMED: &str = "confirmed";
const STATUS_CANDIDATE: &str = "candidate";

const SCORE_CONCEPT_STORE: f32 = 1000.0;
const SCORE_L10N_VALUE_EXACT: f32 = 920.0;
const SCORE_L10N_VALUE_CONTAINS: f32 = 880.0;
const SCORE_L10N_VALUE_FUZZY: f32 = 850.0;
const SCORE_L10N_KEY_EXACT: f32 = 840.0;
const SCORE_L10N_KEY_CONTAINS: f32 = 800.0;
const SCORE_L10N_KEY_FUZZY: f32 = 780.0;
const SCORE_ASSET_EXACT: f32 = 760.0;
const SCORE_ASSET_CONTAINS: f32 = 720.0;
const SCORE_ASSET_FUZZY: f32 = 700.0;
const SCORE_FALLBACK_CALLER_BONUS: f32 = 15.0;
const SCORE_FALLBACK_SEED_PENALTY: f32 = 25.0;
const SCORE_SYMBOL_EXACT: f32 = 660.0;
const SCORE_SYMBOL_PREFIX: f32 = 620.0;
const SCORE_SYMBOL_BM25: f32 = 560.0;
const SCORE_SYMBOL_PRECISE_METADATA_BONUS: f32 = 10.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConceptRecord {
    pub concept: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<ConceptBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConceptBinding {
    pub symbol_id: String,
    #[serde(default = "default_binding_status")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ConceptEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConceptEvidence {
    pub kind: String,
    pub value: String,
    pub match_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConceptSearchResult {
    pub query: String,
    pub resolved_from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_concept: Option<String>,
    pub scopes: Vec<ConceptScopeMatch>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConceptScopeMatch {
    pub symbol: SymbolInfo,
    pub score: f32,
    pub status: String,
    pub evidence: Vec<ConceptEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedConceptSearchResult {
    pub query: String,
    pub resolved_from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_concept: Option<String>,
    pub scopes: Vec<ProjectedConceptScopeMatch>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedConceptScopeMatch {
    pub symbol: ProjectedConceptSymbol,
    pub score: f32,
    pub status: String,
    pub evidence: Vec<ProjectedConceptEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedConceptEvidence {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_query_term: Option<String>,
    pub match_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedConceptSymbol {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<[usize; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<grapha_core::graph::Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<grapha_core::graph::NodeRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<crate::annotations::SymbolAnnotationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConceptShowResult {
    pub query: String,
    pub concept: String,
    pub aliases: Vec<String>,
    pub bindings: Vec<ConceptBindingView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConceptBindingView {
    pub symbol_id: String,
    pub status: String,
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ConceptEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConceptBindResult {
    pub concept: String,
    pub added_bindings: usize,
    pub total_bindings: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConceptAliasResult {
    pub concept: String,
    pub added_aliases: Vec<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConceptRemoveResult {
    pub concept: String,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConceptPruneResult {
    pub pruned_bindings: usize,
    pub touched_concepts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConceptLookup {
    pub concept: String,
    pub matched_term: String,
    pub match_kind: String,
}

#[derive(Debug, Default, Clone)]
pub struct ConceptIndex {
    records: Vec<ConceptRecord>,
    lookup: HashMap<String, ConceptLookupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConceptLookupEntry {
    record_index: usize,
    matched_term: String,
    match_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ConceptSnapshot {
    version: String,
    #[serde(default)]
    concepts: Vec<ConceptRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextMatch {
    Exact,
    Contains,
    Fuzzy,
}

#[derive(Debug)]
struct ScopeAccumulator {
    symbol: SymbolInfo,
    score: f32,
    status: String,
    evidence: Vec<ConceptEvidence>,
    evidence_set: HashSet<ConceptEvidence>,
}

struct ScopeSearchContext<'a> {
    graph: &'a Graph,
    node_index: &'a HashMap<&'a str, &'a Node>,
    parents: &'a HashMap<&'a str, &'a str>,
    edges_by_target: &'a HashMap<&'a str, Vec<&'a Edge>>,
    locators: &'a SymbolLocatorIndex,
    search_index: &'a Index,
    annotations: Option<&'a AnnotationIndex>,
}

#[derive(Debug, Clone, Copy)]
struct SymbolTextQuery<'a> {
    normalized: &'a str,
    raw: &'a str,
}

struct SymbolTextScope<'a, F, N> {
    kind: &'static str,
    value: Option<&'a str>,
    query: SymbolTextQuery<'a>,
    source_value: F,
    note: N,
}

fn default_binding_status() -> String {
    STATUS_CONFIRMED.to_string()
}

impl ConceptSnapshot {
    fn new(mut concepts: Vec<ConceptRecord>) -> Self {
        sort_concepts(&mut concepts);
        Self {
            version: CONCEPTS_SNAPSHOT_VERSION.to_string(),
            concepts,
        }
    }
}

impl ConceptIndex {
    pub fn from_records(records: Vec<ConceptRecord>) -> Self {
        let mut index = Self {
            records,
            lookup: HashMap::new(),
        };
        index.sort_and_rebuild();
        index
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn record_for_term(&self, term: &str) -> Option<(&ConceptRecord, ConceptLookup)> {
        let normalized = normalize_concept(term);
        let entry = self.lookup.get(&normalized)?;
        let record = self.records.get(entry.record_index)?;
        Some((
            record,
            ConceptLookup {
                concept: record.concept.clone(),
                matched_term: entry.matched_term.clone(),
                match_kind: entry.match_kind.clone(),
            },
        ))
    }

    pub fn record_for_search_term(&self, term: &str) -> Option<(&ConceptRecord, ConceptLookup)> {
        self.record_for_term(term)
            .or_else(|| self.fuzzy_record_for_term(term))
    }

    pub fn bind_concept(
        &mut self,
        term: &str,
        symbol_ids: &[String],
        evidence: Vec<ConceptEvidence>,
    ) -> anyhow::Result<ConceptBindResult> {
        let record_index = self.ensure_record(term)?;
        let canonical = self
            .records
            .get(record_index)
            .map(|record| record.concept.clone())
            .unwrap_or_else(|| term.trim().to_string());
        let record = self
            .records
            .get_mut(record_index)
            .expect("record index should exist");
        let mut added_bindings = 0;

        for symbol_id in symbol_ids {
            match record
                .bindings
                .iter_mut()
                .find(|binding| binding.symbol_id == *symbol_id)
            {
                Some(binding) => {
                    merge_evidence(&mut binding.evidence, &evidence);
                    if binding.status.is_empty() {
                        binding.status = STATUS_CONFIRMED.to_string();
                    }
                }
                None => {
                    record.bindings.push(ConceptBinding {
                        symbol_id: symbol_id.clone(),
                        status: STATUS_CONFIRMED.to_string(),
                        evidence: evidence.clone(),
                    });
                    added_bindings += 1;
                }
            }
        }

        self.sort_and_rebuild();
        let total_bindings = self
            .record_for_term(&canonical)
            .map(|(record, _)| record.bindings.len())
            .unwrap_or_default();

        Ok(ConceptBindResult {
            concept: canonical,
            added_bindings,
            total_bindings,
        })
    }

    pub fn add_aliases(
        &mut self,
        term: &str,
        aliases: &[String],
    ) -> anyhow::Result<ConceptAliasResult> {
        let record_index = self.ensure_record(term)?;
        let canonical = self
            .records
            .get(record_index)
            .map(|record| record.concept.clone())
            .unwrap_or_else(|| term.trim().to_string());

        let mut added = Vec::new();
        for alias in aliases {
            let normalized_alias = normalize_concept(alias);
            if normalized_alias.is_empty() {
                continue;
            }

            if let Some(entry) = self.lookup.get(&normalized_alias)
                && entry.record_index != record_index
            {
                bail!(
                    "alias '{}' already belongs to concept '{}'",
                    alias,
                    self.records[entry.record_index].concept
                );
            }

            let record = self
                .records
                .get_mut(record_index)
                .expect("record index should exist");
            if normalize_concept(&record.concept) == normalized_alias
                || record
                    .aliases
                    .iter()
                    .any(|existing| normalize_concept(existing) == normalized_alias)
            {
                continue;
            }

            record.aliases.push(alias.trim().to_string());
            added.push(alias.trim().to_string());
        }

        self.sort_and_rebuild();

        let aliases = self
            .record_for_term(&canonical)
            .map(|(record, _)| record.aliases.clone())
            .unwrap_or_default();

        Ok(ConceptAliasResult {
            concept: canonical,
            added_aliases: added,
            aliases,
        })
    }

    pub fn remove_concept(&mut self, term: &str) -> ConceptRemoveResult {
        let Some((_, lookup)) = self.record_for_term(term) else {
            return ConceptRemoveResult {
                concept: term.trim().to_string(),
                removed: false,
            };
        };
        let normalized = normalize_concept(&lookup.concept);
        let removed =
            if let Some(index) = self.lookup.get(&normalized).map(|entry| entry.record_index) {
                self.records.remove(index);
                true
            } else {
                false
            };
        self.sort_and_rebuild();
        ConceptRemoveResult {
            concept: lookup.concept,
            removed,
        }
    }

    pub fn prune(&mut self, valid_ids: &HashSet<&str>) -> ConceptPruneResult {
        let mut pruned_bindings = 0;
        let mut touched_concepts = 0;

        for record in &mut self.records {
            let before = record.bindings.len();
            record
                .bindings
                .retain(|binding| valid_ids.contains(binding.symbol_id.as_str()));
            let removed = before.saturating_sub(record.bindings.len());
            if removed > 0 {
                pruned_bindings += removed;
                touched_concepts += 1;
            }
        }

        self.sort_and_rebuild();

        ConceptPruneResult {
            pruned_bindings,
            touched_concepts,
        }
    }

    fn ensure_record(&mut self, term: &str) -> anyhow::Result<usize> {
        let normalized = normalize_concept(term);
        if normalized.is_empty() {
            bail!("concept term cannot be empty");
        }

        if let Some(entry) = self.lookup.get(&normalized) {
            return Ok(entry.record_index);
        }

        self.records.push(ConceptRecord {
            concept: term.trim().to_string(),
            aliases: Vec::new(),
            bindings: Vec::new(),
            notes: None,
        });
        self.sort_and_rebuild();
        self.lookup
            .get(&normalized)
            .map(|entry| entry.record_index)
            .context("new concept should be indexed")
    }

    fn sort_and_rebuild(&mut self) {
        sort_concepts(&mut self.records);
        self.lookup.clear();
        for (index, record) in self.records.iter().enumerate() {
            let normalized_concept = normalize_concept(&record.concept);
            if !normalized_concept.is_empty() {
                self.lookup.insert(
                    normalized_concept,
                    ConceptLookupEntry {
                        record_index: index,
                        matched_term: record.concept.clone(),
                        match_kind: "concept".to_string(),
                    },
                );
            }
            for alias in &record.aliases {
                let normalized_alias = normalize_concept(alias);
                if normalized_alias.is_empty() {
                    continue;
                }
                self.lookup.insert(
                    normalized_alias,
                    ConceptLookupEntry {
                        record_index: index,
                        matched_term: alias.clone(),
                        match_kind: "alias".to_string(),
                    },
                );
            }
        }
    }

    fn fuzzy_record_for_term(&self, term: &str) -> Option<(&ConceptRecord, ConceptLookup)> {
        let normalized = normalize_concept(term);
        if normalized.is_empty() {
            return None;
        }

        let mut best: Option<(&ConceptLookupEntry, usize, usize)> = None;
        for (candidate, entry) in &self.lookup {
            let Some((distance, length_delta)) = fuzzy_candidate_rank(&normalized, candidate)
            else {
                continue;
            };
            let should_replace = match best.as_ref() {
                Some((best_entry, best_distance, best_length_delta)) => {
                    (distance, length_delta, entry.matched_term.as_str())
                        < (
                            *best_distance,
                            *best_length_delta,
                            best_entry.matched_term.as_str(),
                        )
                }
                None => true,
            };
            if should_replace {
                best = Some((entry, distance, length_delta));
            }
        }

        let (entry, _, _) = best?;
        let record = self.records.get(entry.record_index)?;
        Some((
            record,
            ConceptLookup {
                concept: record.concept.clone(),
                matched_term: entry.matched_term.clone(),
                match_kind: format!("fuzzy_{}", entry.match_kind),
            },
        ))
    }
}

pub fn load_concept_index(project_root: &Path) -> anyhow::Result<ConceptIndex> {
    load_concept_index_from_store(&project_root.join(".grapha"))
}

pub fn load_concept_index_from_store(store_dir: &Path) -> anyhow::Result<ConceptIndex> {
    let path = snapshot_path(store_dir);
    if !path.exists() {
        return Ok(ConceptIndex::default());
    }

    let payload = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let snapshot: ConceptSnapshot = serde_json::from_str(&payload)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if snapshot.version != CONCEPTS_SNAPSHOT_VERSION {
        bail!(
            "unsupported concept snapshot version: {} (expected {})",
            snapshot.version,
            CONCEPTS_SNAPSHOT_VERSION
        );
    }
    Ok(ConceptIndex::from_records(snapshot.concepts))
}

pub fn save_concept_index(project_root: &Path, index: &ConceptIndex) -> anyhow::Result<()> {
    save_concept_index_to_store(&project_root.join(".grapha"), index)
}

pub fn save_concept_index_to_store(store_dir: &Path, index: &ConceptIndex) -> anyhow::Result<()> {
    std::fs::create_dir_all(store_dir)
        .with_context(|| format!("failed to create store dir {}", store_dir.display()))?;
    let path = snapshot_path(store_dir);
    let snapshot = ConceptSnapshot::new(index.records.clone());
    let payload = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn show_concept(
    graph: &Graph,
    concepts: &ConceptIndex,
    term: &str,
) -> anyhow::Result<ConceptShowResult> {
    let Some((record, _)) = concepts.record_for_term(term) else {
        bail!("concept not found: {}", term.trim());
    };
    let node_index = graph_node_index(graph);
    let locators = SymbolLocatorIndex::new(graph);

    let mut bindings = Vec::new();
    for binding in &record.bindings {
        let symbol = node_index
            .get(binding.symbol_id.as_str())
            .copied()
            .map(|node| symbol_info(node, &locators));
        bindings.push(ConceptBindingView {
            symbol_id: binding.symbol_id.clone(),
            status: binding.status.clone(),
            stale: symbol.is_none(),
            symbol,
            evidence: binding.evidence.clone(),
        });
    }

    bindings.sort_by(|left, right| {
        left.stale
            .cmp(&right.stale)
            .then_with(|| left.status.cmp(&right.status))
            .then_with(|| left.symbol_id.cmp(&right.symbol_id))
    });

    Ok(ConceptShowResult {
        query: term.trim().to_string(),
        concept: record.concept.clone(),
        aliases: record.aliases.clone(),
        bindings,
        notes: record.notes.clone(),
    })
}

#[cfg(test)]
pub fn search_concepts(
    graph: &Graph,
    search_index: &Index,
    concepts: &ConceptIndex,
    catalogs: &LocalizationCatalogIndex,
    assets_index: &AssetCatalogIndex,
    query: &str,
    limit: usize,
) -> anyhow::Result<ConceptSearchResult> {
    search_concepts_with_annotations(
        graph,
        search_index,
        concepts,
        catalogs,
        assets_index,
        query,
        limit,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn search_concepts_with_annotations(
    graph: &Graph,
    search_index: &Index,
    concepts: &ConceptIndex,
    catalogs: &LocalizationCatalogIndex,
    assets_index: &AssetCatalogIndex,
    query: &str,
    limit: usize,
    annotations: Option<&AnnotationIndex>,
) -> anyhow::Result<ConceptSearchResult> {
    let locators = SymbolLocatorIndex::new(graph);
    let node_index = graph_node_index(graph);
    let parents = contains_parents(graph);
    let edges_by_target = graph_edges_by_target(graph);
    let scope_context = ScopeSearchContext {
        graph,
        node_index: &node_index,
        parents: &parents,
        edges_by_target: &edges_by_target,
        locators: &locators,
        search_index,
        annotations,
    };

    if let Some((record, lookup)) = concepts.record_for_search_term(query) {
        let mut scopes = direct_concept_scopes(record, &lookup, &node_index, &locators, limit);
        if !scopes.is_empty() {
            attach_scope_annotations(&mut scopes, &node_index, annotations);
            return Ok(ConceptSearchResult {
                query: query.trim().to_string(),
                resolved_from: "concept_store".to_string(),
                matched_concept: Some(record.concept.clone()),
                scopes,
            });
        }
    }

    let mut scopes = HashMap::<String, ScopeAccumulator>::new();
    let normalized_query = normalize_match_text(query);

    add_localization_value_scopes(
        &mut scopes,
        &scope_context,
        catalogs,
        &normalized_query,
        query,
        TextMatch::Exact,
    );
    add_localization_value_scopes(
        &mut scopes,
        &scope_context,
        catalogs,
        &normalized_query,
        query,
        TextMatch::Contains,
    );
    add_localization_value_scopes(
        &mut scopes,
        &scope_context,
        catalogs,
        &normalized_query,
        query,
        TextMatch::Fuzzy,
    );
    add_localization_key_scopes(
        &mut scopes,
        &scope_context,
        catalogs,
        &normalized_query,
        query,
        TextMatch::Exact,
    );
    add_localization_key_scopes(
        &mut scopes,
        &scope_context,
        catalogs,
        &normalized_query,
        query,
        TextMatch::Contains,
    );
    add_localization_key_scopes(
        &mut scopes,
        &scope_context,
        catalogs,
        &normalized_query,
        query,
        TextMatch::Fuzzy,
    );
    add_asset_scopes(
        &mut scopes,
        &scope_context,
        assets_index,
        &normalized_query,
        TextMatch::Exact,
    );
    add_asset_scopes(
        &mut scopes,
        &scope_context,
        assets_index,
        &normalized_query,
        TextMatch::Contains,
    );
    add_asset_scopes(
        &mut scopes,
        &scope_context,
        assets_index,
        &normalized_query,
        TextMatch::Fuzzy,
    );
    add_symbol_metadata_scopes(&mut scopes, &scope_context, &normalized_query, query);
    add_symbol_scopes(&mut scopes, &scope_context, query, limit)?;

    let mut matches: Vec<_> = scopes
        .into_values()
        .map(|scope| ConceptScopeMatch {
            symbol: scope.symbol,
            score: scope.score + ((scope.evidence.len().saturating_sub(1)) as f32 * 5.0),
            status: scope.status,
            evidence: scope.evidence,
        })
        .collect();
    attach_scope_annotations(&mut matches, &node_index, annotations);
    matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.symbol.name.cmp(&right.symbol.name))
            .then_with(|| left.symbol.file.cmp(&right.symbol.file))
    });
    matches.truncate(limit);

    Ok(ConceptSearchResult {
        query: query.trim().to_string(),
        resolved_from: "heuristics".to_string(),
        matched_concept: None,
        scopes: matches,
    })
}

pub fn project_concept_search_result(
    result: &ConceptSearchResult,
    fields: FieldSet,
) -> ProjectedConceptSearchResult {
    ProjectedConceptSearchResult {
        query: result.query.clone(),
        resolved_from: result.resolved_from.clone(),
        matched_concept: result.matched_concept.clone(),
        scopes: result
            .scopes
            .iter()
            .map(|scope| ProjectedConceptScopeMatch {
                symbol: project_concept_symbol(&scope.symbol, fields),
                score: scope.score,
                status: scope.status.clone(),
                evidence: project_concept_evidence(
                    &scope.evidence,
                    &result.query,
                    &scope.symbol.name,
                    fields == FieldSet::all(),
                ),
            })
            .collect(),
    }
}

fn project_concept_evidence(
    evidence: &[ConceptEvidence],
    query: &str,
    symbol_name: &str,
    full: bool,
) -> Vec<ProjectedConceptEvidence> {
    let mut projected = evidence
        .iter()
        .map(|evidence| ProjectedConceptEvidence {
            kind: evidence.kind.clone(),
            value: full.then(|| evidence.value.clone()),
            matched_query_term: (!full && evidence.value.trim() != query.trim())
                .then(|| evidence.value.clone()),
            match_kind: evidence.match_kind.clone(),
            table: evidence.table.clone(),
            key: evidence.key.clone(),
            source_value: evidence.source_value.clone(),
            ui_path: evidence.ui_path.clone(),
            note: (full || evidence.note.as_deref() != Some(symbol_name))
                .then(|| evidence.note.clone())
                .flatten(),
        })
        .collect::<Vec<_>>();

    if !full {
        remove_redundant_snippet_evidence(&mut projected);
    }

    projected
}

fn remove_redundant_snippet_evidence(evidence: &mut Vec<ProjectedConceptEvidence>) {
    let redundant_snippet_indexes = evidence
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            (candidate.kind == "snippet"
                && evidence.iter().enumerate().any(|(other_index, other)| {
                    other_index != index
                        && other.kind != "snippet"
                        && same_concise_evidence_match(candidate, other)
                        && source_contains(candidate, other)
                }))
            .then_some(index)
        })
        .collect::<HashSet<_>>();

    if redundant_snippet_indexes.is_empty() {
        return;
    }

    let mut index = 0usize;
    evidence.retain(|_| {
        let keep = !redundant_snippet_indexes.contains(&index);
        index += 1;
        keep
    });
}

fn same_concise_evidence_match(
    left: &ProjectedConceptEvidence,
    right: &ProjectedConceptEvidence,
) -> bool {
    left.value == right.value
        && left.matched_query_term == right.matched_query_term
        && left.match_kind == right.match_kind
        && left.table == right.table
        && left.key == right.key
        && left.ui_path == right.ui_path
        && left.note == right.note
}

fn source_contains(
    container: &ProjectedConceptEvidence,
    contained: &ProjectedConceptEvidence,
) -> bool {
    match (
        container.source_value.as_deref(),
        contained.source_value.as_deref(),
    ) {
        (Some(container), Some(contained)) => {
            !contained.is_empty() && container != contained && container.contains(contained)
        }
        _ => false,
    }
}

fn project_concept_symbol(symbol: &SymbolInfo, fields: FieldSet) -> ProjectedConceptSymbol {
    ProjectedConceptSymbol {
        id: fields.id.then(|| symbol.id.clone()),
        locator: if fields.locator {
            symbol.locator.clone()
        } else {
            None
        },
        name: symbol.name.clone(),
        kind: symbol.kind,
        file: fields.file.then(|| symbol.file.clone()),
        span: fields.span.then_some(symbol.span),
        visibility: fields.visibility.then_some(symbol.visibility).flatten(),
        role: if fields.role {
            symbol.role.clone()
        } else {
            None
        },
        signature: if fields.signature {
            symbol.signature.clone()
        } else {
            None
        },
        doc_comment: if fields.doc_comment {
            symbol.doc_comment.clone()
        } else {
            None
        },
        annotation: if fields.annotation {
            symbol.annotation.clone()
        } else {
            None
        },
        module: if fields.module {
            symbol.module.clone()
        } else {
            None
        },
        snippet: if fields.snippet {
            symbol.snippet.clone()
        } else {
            None
        },
        repo: if fields.repo {
            symbol.repo.clone()
        } else {
            None
        },
    }
}

fn direct_concept_scopes(
    record: &ConceptRecord,
    lookup: &ConceptLookup,
    node_index: &HashMap<&str, &Node>,
    locators: &SymbolLocatorIndex,
    limit: usize,
) -> Vec<ConceptScopeMatch> {
    let mut scopes = Vec::new();
    for binding in &record.bindings {
        let Some(node) = node_index.get(binding.symbol_id.as_str()).copied() else {
            continue;
        };
        let mut evidence = vec![ConceptEvidence {
            kind: "concept_binding".to_string(),
            value: lookup.matched_term.clone(),
            match_kind: lookup.match_kind.clone(),
            table: None,
            key: None,
            source_value: None,
            ui_path: Vec::new(),
            note: Some(record.concept.clone()),
        }];
        merge_evidence(&mut evidence, &binding.evidence);
        scopes.push(ConceptScopeMatch {
            symbol: symbol_info(node, locators),
            score: SCORE_CONCEPT_STORE,
            status: if binding.status.is_empty() {
                STATUS_CONFIRMED.to_string()
            } else {
                binding.status.clone()
            },
            evidence,
        });
    }
    scopes.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.symbol.name.cmp(&right.symbol.name))
    });
    scopes.truncate(limit);
    scopes
}

fn add_localization_value_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    catalogs: &LocalizationCatalogIndex,
    normalized_query: &str,
    raw_query: &str,
    match_type: TextMatch,
) {
    let mut seen_records = HashSet::new();
    for record in catalogs.all_records() {
        if !matches_localization_value(record, normalized_query, match_type) {
            continue;
        }
        let record_key = (
            record.table.clone(),
            record.key.clone(),
            record.catalog_file.clone(),
        );
        if !seen_records.insert(record_key) {
            continue;
        }

        add_record_usage_scopes(
            scopes,
            context,
            catalogs,
            record,
            if match_type == TextMatch::Exact {
                SCORE_L10N_VALUE_EXACT
            } else if match_type == TextMatch::Fuzzy {
                SCORE_L10N_VALUE_FUZZY
            } else {
                SCORE_L10N_VALUE_CONTAINS
            },
            ConceptEvidence {
                kind: "l10n_value".to_string(),
                value: raw_query.trim().to_string(),
                match_kind: match_kind_label(match_type).to_string(),
                table: Some(record.table.clone()),
                key: Some(record.key.clone()),
                source_value: Some(record.source_value.clone()),
                ui_path: Vec::new(),
                note: None,
            },
        );
    }
}

fn add_localization_key_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    catalogs: &LocalizationCatalogIndex,
    normalized_query: &str,
    raw_query: &str,
    match_type: TextMatch,
) {
    let mut seen_records = HashSet::new();
    for record in catalogs.all_records() {
        if !matches_localization_key(record, normalized_query, match_type) {
            continue;
        }
        let record_key = (
            record.table.clone(),
            record.key.clone(),
            record.catalog_file.clone(),
        );
        if !seen_records.insert(record_key) {
            continue;
        }

        add_record_usage_scopes(
            scopes,
            context,
            catalogs,
            record,
            if match_type == TextMatch::Exact {
                SCORE_L10N_KEY_EXACT
            } else if match_type == TextMatch::Fuzzy {
                SCORE_L10N_KEY_FUZZY
            } else {
                SCORE_L10N_KEY_CONTAINS
            },
            ConceptEvidence {
                kind: "l10n_key".to_string(),
                value: raw_query.trim().to_string(),
                match_kind: match_kind_label(match_type).to_string(),
                table: Some(record.table.clone()),
                key: Some(record.key.clone()),
                source_value: Some(record.source_value.clone()),
                ui_path: Vec::new(),
                note: None,
            },
        );
    }
}

fn add_record_usage_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    catalogs: &LocalizationCatalogIndex,
    record: &LocalizationCatalogRecord,
    score: f32,
    base_evidence: ConceptEvidence,
) {
    let result =
        query::usages::query_usages(context.graph, catalogs, &record.key, Some(&record.table));
    let mut usage_count = 0;
    for record_group in result.records {
        if record_group.record.table != record.table || record_group.record.key != record.key {
            continue;
        }
        for usage in record_group.usages {
            usage_count += 1;
            let Some(scope_node) = context.node_index.get(usage.owner.id.as_str()).copied() else {
                continue;
            };
            let mut evidence = base_evidence.clone();
            evidence.ui_path = usage.ui_path.clone();
            add_scope(
                scopes,
                scope_node,
                context.locators,
                score,
                STATUS_CANDIDATE,
                evidence,
            );
        }
    }

    if usage_count == 0 {
        add_l10n_fallback_scopes(scopes, context, record, score, &base_evidence);
    }
}

fn add_asset_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    assets_index: &AssetCatalogIndex,
    normalized_query: &str,
    match_type: TextMatch,
) {
    let mut seen_records = HashSet::new();
    for record in assets_index.all_records() {
        if !matches_asset_name(record, normalized_query, match_type) {
            continue;
        }
        let record_key = (
            record.catalog.clone(),
            record.catalog_dir.clone(),
            record.name.clone(),
        );
        if !seen_records.insert(record_key) {
            continue;
        }

        let mut usage_count = 0;
        for usage in assets::find_usages(context.graph, &record.name) {
            usage_count += 1;
            let Some(node) = context.node_index.get(usage.node_id.as_str()).copied() else {
                continue;
            };
            let scope = scope_for_node(node, context.parents, context.node_index);
            add_scope(
                scopes,
                scope,
                context.locators,
                if match_type == TextMatch::Exact {
                    SCORE_ASSET_EXACT
                } else if match_type == TextMatch::Fuzzy {
                    SCORE_ASSET_FUZZY
                } else {
                    SCORE_ASSET_CONTAINS
                },
                STATUS_CANDIDATE,
                ConceptEvidence {
                    kind: "asset_name".to_string(),
                    value: record.name.clone(),
                    match_kind: match_kind_label(match_type).to_string(),
                    table: None,
                    key: None,
                    source_value: None,
                    ui_path: Vec::new(),
                    note: Some(record.catalog.clone()),
                },
            );
        }

        if usage_count == 0 {
            add_asset_fallback_scopes(
                scopes,
                context,
                record,
                if match_type == TextMatch::Exact {
                    SCORE_ASSET_EXACT
                } else if match_type == TextMatch::Fuzzy {
                    SCORE_ASSET_FUZZY
                } else {
                    SCORE_ASSET_CONTAINS
                },
            );
        }
    }
}

fn add_symbol_metadata_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    normalized_query: &str,
    raw_query: &str,
) {
    let query = SymbolTextQuery {
        normalized: normalized_query,
        raw: raw_query,
    };

    for node in &context.graph.nodes {
        add_symbol_text_scope(
            scopes,
            context,
            node,
            SymbolTextScope {
                kind: "doc_comment",
                value: node.doc_comment.as_deref(),
                query,
                source_value: |value: &str| value.to_string(),
                note: || Some(node.name.clone()),
            },
        );
        add_symbol_text_scope(
            scopes,
            context,
            node,
            SymbolTextScope {
                kind: "snippet",
                value: should_match_concept_snippet(node.kind)
                    .then_some(node.snippet.as_deref())
                    .flatten(),
                query,
                source_value: compact_symbol_snippet,
                note: || Some(node.name.clone()),
            },
        );
        if let Some(annotation) = context
            .annotations
            .and_then(|index| index.get_for_node(node))
        {
            let note = if annotation.stale {
                format!("{} (stale)", node.name)
            } else {
                node.name.clone()
            };
            add_symbol_text_scope(
                scopes,
                context,
                node,
                SymbolTextScope {
                    kind: "annotation",
                    value: Some(annotation.text.as_str()),
                    query,
                    source_value: |value: &str| value.to_string(),
                    note: || Some(note.clone()),
                },
            );
        }
    }
}

fn add_symbol_text_scope<F, N>(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    node: &Node,
    text_scope: SymbolTextScope<'_, F, N>,
) where
    F: Fn(&str) -> String,
    N: Fn() -> Option<String>,
{
    let Some(value) = text_scope.value else {
        return;
    };
    let Some((match_kind, base_score)) = concept_doc_match(value, text_scope.query.normalized)
    else {
        return;
    };
    let score = match text_scope.kind {
        "doc_comment" | "annotation" => base_score + SCORE_SYMBOL_PRECISE_METADATA_BONUS,
        _ => base_score,
    };
    add_scope(
        scopes,
        node,
        context.locators,
        score,
        STATUS_CANDIDATE,
        ConceptEvidence {
            kind: text_scope.kind.to_string(),
            value: text_scope.query.raw.trim().to_string(),
            match_kind: match_kind.to_string(),
            table: None,
            key: None,
            source_value: Some((text_scope.source_value)(value)),
            ui_path: Vec::new(),
            note: (text_scope.note)(),
        },
    );
}

fn should_match_concept_snippet(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File
            | NodeKind::Module
            | NodeKind::Namespace
            | NodeKind::Import
            | NodeKind::Export
    )
}

fn add_symbol_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    query: &str,
    limit: usize,
) -> anyhow::Result<()> {
    let mut results = search::search_filtered(
        context.search_index,
        query,
        limit.saturating_mul(4).max(8),
        &SearchOptions::default(),
    )?;
    let mut seen_result_ids: HashSet<String> =
        results.iter().map(|result| result.id.clone()).collect();
    // The fuzzy pass is a best-effort ranking supplement layered on top of the
    // exact pass and the comment/snippet scan in `add_symbol_metadata_scopes`.
    // It must never be able to fail the whole concept search: propagating its
    // error here would discard results those other arms already found. A query
    // shaped unlike a symbol name (prose, a whole question) is exactly the case
    // that can trip regex compilation while the substring arms succeed.
    match search::search_filtered(
        context.search_index,
        query,
        limit.saturating_mul(4).max(8),
        &SearchOptions {
            fuzzy: true,
            ..SearchOptions::default()
        },
    ) {
        Ok(fuzzy_results) => {
            for result in fuzzy_results {
                if seen_result_ids.insert(result.id.clone()) {
                    results.push(result);
                }
            }
        }
        Err(_) => {
            // Deliberately silent: this function has no verbose flag, and the
            // exact pass plus the metadata scan still carry the result set.
            // `build_fuzzy_regex` caps the pattern so compilation should not
            // fail here; this arm is the backstop that keeps any future failure
            // from turning a degraded ranking into a failed search.
        }
    }
    let normalized_query = normalize_match_text(query);

    for (rank, result) in results.iter().enumerate() {
        let Some(node) = context.node_index.get(result.id.as_str()).copied() else {
            continue;
        };
        let scope = scope_for_node(node, context.parents, context.node_index);
        let normalized_name = normalize_match_text(query::normalize_symbol_name(&node.name));
        let evidence = if let Some((match_kind, base_score)) =
            concept_symbol_match(&normalized_name, &normalized_query)
        {
            (
                base_score,
                ConceptEvidence {
                    kind: "symbol_query".to_string(),
                    value: query.trim().to_string(),
                    match_kind: match_kind.to_string(),
                    table: None,
                    key: None,
                    source_value: None,
                    ui_path: Vec::new(),
                    note: Some(node.name.clone()),
                },
            )
        } else if let Some(doc_comment) = node.doc_comment.as_deref()
            && let Some((match_kind, base_score)) =
                concept_doc_match(doc_comment, &normalized_query)
        {
            (
                base_score,
                ConceptEvidence {
                    kind: "doc_comment".to_string(),
                    value: query.trim().to_string(),
                    match_kind: match_kind.to_string(),
                    table: None,
                    key: None,
                    source_value: Some(doc_comment.to_string()),
                    ui_path: Vec::new(),
                    note: Some(node.name.clone()),
                },
            )
        } else if let Some(annotation) = context
            .annotations
            .and_then(|index| index.get_for_node(node))
            && let Some((match_kind, base_score)) =
                concept_doc_match(&annotation.text, &normalized_query)
        {
            (
                base_score,
                ConceptEvidence {
                    kind: "annotation".to_string(),
                    value: query.trim().to_string(),
                    match_kind: match_kind.to_string(),
                    table: None,
                    key: None,
                    source_value: Some(annotation.text),
                    ui_path: Vec::new(),
                    note: Some(if annotation.stale {
                        format!("{} (stale)", node.name)
                    } else {
                        node.name.clone()
                    }),
                },
            )
        } else {
            continue;
        };
        add_scope(
            scopes,
            scope,
            context.locators,
            (evidence.0 - rank as f32).max(0.0),
            STATUS_CANDIDATE,
            evidence.1,
        );
    }
    Ok(())
}

fn concept_symbol_match(
    normalized_name: &str,
    normalized_query: &str,
) -> Option<(&'static str, f32)> {
    if normalized_query.is_empty() {
        return None;
    }
    if normalized_name == normalized_query {
        Some(("exact", SCORE_SYMBOL_EXACT))
    } else if normalized_name.starts_with(normalized_query) {
        Some(("prefix", SCORE_SYMBOL_PREFIX))
    } else if normalized_name.contains(normalized_query) {
        Some(("contains", SCORE_SYMBOL_PREFIX))
    } else if fuzzy_query_tokens_match(normalized_query, normalized_name) {
        Some(("fuzzy", SCORE_SYMBOL_BM25))
    } else {
        None
    }
}

fn concept_doc_match(doc_comment: &str, normalized_query: &str) -> Option<(&'static str, f32)> {
    let normalized_doc = normalize_match_text(doc_comment);
    if normalized_query.is_empty() || normalized_doc.is_empty() {
        return None;
    }
    if normalized_doc == normalized_query {
        Some(("exact", SCORE_SYMBOL_BM25))
    } else if normalized_doc.contains(normalized_query) {
        Some(("contains", SCORE_SYMBOL_BM25))
    } else if fuzzy_query_tokens_match(normalized_query, &normalized_doc) {
        Some(("fuzzy", SCORE_SYMBOL_BM25))
    } else {
        None
    }
}

fn add_l10n_fallback_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    record: &LocalizationCatalogRecord,
    score: f32,
    base_evidence: &ConceptEvidence,
) {
    let queries = l10n_symbol_queries(record);
    add_seed_symbol_scopes(
        scopes,
        context,
        &queries,
        score,
        |candidate, node_name, is_caller| ConceptEvidence {
            kind: "l10n_wrapper".to_string(),
            value: candidate.to_string(),
            match_kind: if is_caller {
                "wrapper_caller".to_string()
            } else {
                "wrapper_symbol".to_string()
            },
            table: base_evidence.table.clone(),
            key: base_evidence.key.clone(),
            source_value: base_evidence.source_value.clone(),
            ui_path: Vec::new(),
            note: Some(node_name.to_string()),
        },
    );
}

fn add_asset_fallback_scopes(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    record: &AssetRecord,
    score: f32,
) {
    let queries = asset_symbol_queries(record);
    add_seed_symbol_scopes(
        scopes,
        context,
        &queries,
        score,
        |candidate, node_name, is_caller| ConceptEvidence {
            kind: "asset_wrapper".to_string(),
            value: candidate.to_string(),
            match_kind: if is_caller {
                "wrapper_caller".to_string()
            } else {
                "wrapper_symbol".to_string()
            },
            table: None,
            key: None,
            source_value: None,
            ui_path: Vec::new(),
            note: Some(node_name.to_string()),
        },
    );
}

fn add_seed_symbol_scopes<F>(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    context: &ScopeSearchContext<'_>,
    queries: &[String],
    score: f32,
    evidence_builder: F,
) where
    F: Fn(&str, &str, bool) -> ConceptEvidence,
{
    let mut seen_seed_ids = HashSet::new();
    for query in queries {
        let normalized_query = normalize_match_text(query);
        if normalized_query.is_empty() {
            continue;
        }

        let Ok(results) =
            search::search_filtered(context.search_index, query, 8, &SearchOptions::default())
        else {
            continue;
        };
        let matching_seeds: Vec<&Node> = results
            .into_iter()
            .filter_map(|result| context.node_index.get(result.id.as_str()).copied())
            .filter(|seed| seed_matches_query(seed, query, &normalized_query))
            .collect();
        let preferred_non_accessor_bases: HashSet<String> = matching_seeds
            .iter()
            .copied()
            .filter(|seed| !is_accessor_symbol(seed))
            .map(|seed| normalize_match_text(query::normalize_symbol_name(&seed.name)))
            .collect();

        for seed in matching_seeds {
            if is_accessor_symbol(seed)
                && preferred_non_accessor_bases.contains(&normalize_match_text(
                    query::normalize_symbol_name(&seed.name),
                ))
            {
                continue;
            }
            if !seen_seed_ids.insert(seed.id.clone()) {
                continue;
            }

            let seed_scope = seed_scope_for_node(seed, context.parents, context.node_index);
            add_scope(
                scopes,
                seed_scope,
                context.locators,
                (score - SCORE_FALLBACK_SEED_PENALTY).max(0.0),
                STATUS_CANDIDATE,
                evidence_builder(query, &seed.name, false),
            );

            for caller in related_caller_nodes(
                seed.id.as_str(),
                context.edges_by_target,
                context.node_index,
            ) {
                let caller_scope = scope_for_node(caller, context.parents, context.node_index);
                if should_skip_generated_container_scope(seed, caller_scope) {
                    continue;
                }
                add_scope(
                    scopes,
                    caller_scope,
                    context.locators,
                    score + SCORE_FALLBACK_CALLER_BONUS,
                    STATUS_CANDIDATE,
                    evidence_builder(query, &caller.name, true),
                );
            }
        }
    }
}

fn add_scope(
    scopes: &mut HashMap<String, ScopeAccumulator>,
    node: &Node,
    locators: &SymbolLocatorIndex,
    score: f32,
    status: &str,
    evidence: ConceptEvidence,
) {
    let symbol = symbol_info(node, locators);
    match scopes.get_mut(symbol.id.as_str()) {
        Some(existing) => {
            if score > existing.score {
                existing.score = score;
            }
            if existing.status != STATUS_CONFIRMED && status == STATUS_CONFIRMED {
                existing.status = status.to_string();
            }
            if existing.evidence_set.insert(evidence.clone()) {
                existing.evidence.push(evidence);
            }
        }
        None => {
            let mut evidence_set = HashSet::new();
            evidence_set.insert(evidence.clone());
            scopes.insert(
                symbol.id.clone(),
                ScopeAccumulator {
                    symbol,
                    score,
                    status: status.to_string(),
                    evidence: vec![evidence],
                    evidence_set,
                },
            );
        }
    }
}

fn scope_for_node<'a>(
    node: &'a Node,
    parents: &HashMap<&'a str, &'a str>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> &'a Node {
    match node.kind {
        NodeKind::Branch => {
            first_non_branch_ancestor(node.id.as_str(), parents, node_index).unwrap_or(node)
        }
        NodeKind::Property | NodeKind::Field | NodeKind::Variant => {
            first_scope_ancestor(node.id.as_str(), parents, node_index).unwrap_or(node)
        }
        NodeKind::Function => {
            let Some(parent) = first_non_branch_ancestor(node.id.as_str(), parents, node_index)
            else {
                return node;
            };
            if matches!(
                parent.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Impl
                    | NodeKind::Extension
                    | NodeKind::View
            ) {
                parent
            } else {
                node
            }
        }
        _ => node,
    }
}

fn seed_scope_for_node<'a>(
    node: &'a Node,
    parents: &HashMap<&'a str, &'a str>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> &'a Node {
    let file_name = node
        .file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if file_name.ends_with("Strings.generated.swift")
        || file_name.ends_with("Assets.generated.swift")
    {
        return node;
    }
    scope_for_node(node, parents, node_index)
}

fn first_non_branch_ancestor<'a>(
    node_id: &'a str,
    parents: &HashMap<&'a str, &'a str>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Option<&'a Node> {
    let mut current = parents.get(node_id).copied();
    while let Some(id) = current {
        let node = node_index.get(id).copied()?;
        if node.kind != NodeKind::Branch {
            return Some(node);
        }
        current = parents.get(id).copied();
    }
    None
}

fn first_scope_ancestor<'a>(
    node_id: &'a str,
    parents: &HashMap<&'a str, &'a str>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Option<&'a Node> {
    let mut current = parents.get(node_id).copied();
    while let Some(id) = current {
        let node = node_index.get(id).copied()?;
        if matches!(
            node.kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Trait
                | NodeKind::Protocol
                | NodeKind::Impl
                | NodeKind::Extension
                | NodeKind::View
                | NodeKind::Function
        ) {
            return Some(node);
        }
        if node.kind != NodeKind::Branch && node.kind != NodeKind::Module {
            return Some(node);
        }
        current = parents.get(id).copied();
    }
    None
}

fn attach_scope_annotations(
    scopes: &mut [ConceptScopeMatch],
    node_index: &HashMap<&str, &Node>,
    annotations: Option<&AnnotationIndex>,
) {
    let Some(annotations) = annotations else {
        return;
    };
    for scope in scopes {
        let Some(node) = node_index.get(scope.symbol.id.as_str()).copied() else {
            continue;
        };
        scope.symbol.annotation = annotations.get_for_node(node);
    }
}

fn symbol_info(node: &Node, locators: &SymbolLocatorIndex) -> SymbolInfo {
    let locator = locators.locator_for_id(&node.id);
    let mut info = SymbolInfo::from_node(node);
    info.snippet = info.snippet.as_deref().map(compact_symbol_snippet);
    match locator {
        Some(locator) => info.with_locator(locator.to_string()),
        None => info,
    }
}

fn contains_parents(graph: &Graph) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for edge in &graph.edges {
        if edge.kind == EdgeKind::Contains {
            map.insert(edge.target.as_str(), edge.source.as_str());
        }
    }
    map
}

fn graph_edges_by_target(graph: &Graph) -> HashMap<&str, Vec<&Edge>> {
    let mut map: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        map.entry(edge.target.as_str()).or_default().push(edge);
    }
    map
}

fn graph_node_index(graph: &Graph) -> HashMap<&str, &Node> {
    graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect()
}

fn matches_localization_value(
    record: &LocalizationCatalogRecord,
    normalized_query: &str,
    match_type: TextMatch,
) -> bool {
    if normalized_query.is_empty() {
        return false;
    }
    localization_values(record).into_iter().any(|value| {
        let normalized_value = normalize_match_text(value);
        match match_type {
            TextMatch::Exact => normalized_value == normalized_query,
            TextMatch::Contains => {
                normalized_value.contains(normalized_query) && normalized_value != normalized_query
            }
            TextMatch::Fuzzy => fuzzy_matches_text(&normalized_value, normalized_query),
        }
    })
}

fn matches_localization_key(
    record: &LocalizationCatalogRecord,
    normalized_query: &str,
    match_type: TextMatch,
) -> bool {
    matches_text(record.key.as_str(), normalized_query, match_type)
}

fn matches_asset_name(record: &AssetRecord, normalized_query: &str, match_type: TextMatch) -> bool {
    matches_text(record.name.as_str(), normalized_query, match_type)
}

fn matches_text(value: &str, normalized_query: &str, match_type: TextMatch) -> bool {
    if normalized_query.is_empty() {
        return false;
    }
    let normalized_value = normalize_match_text(value);
    match match_type {
        TextMatch::Exact => normalized_value == normalized_query,
        TextMatch::Contains => {
            normalized_value.contains(normalized_query) && normalized_value != normalized_query
        }
        TextMatch::Fuzzy => fuzzy_matches_text(&normalized_value, normalized_query),
    }
}

fn localization_values(record: &LocalizationCatalogRecord) -> Vec<&str> {
    let mut values = Vec::new();
    if !record.source_value.is_empty() {
        values.push(record.source_value.as_str());
    }
    values.extend(
        record
            .translations
            .values()
            .filter(|value| !value.is_empty())
            .map(String::as_str),
    );
    values
}

fn seed_matches_query(node: &Node, raw_query: &str, normalized_query: &str) -> bool {
    let normalized_name = normalize_match_text(query::normalize_symbol_name(&node.name));
    if normalized_name == normalized_query || normalized_name.contains(normalized_query) {
        return true;
    }

    let snippet = node.snippet.as_deref().unwrap_or_default();
    snippet.contains(raw_query)
}

fn related_caller_nodes<'a>(
    seed_id: &'a str,
    edges_by_target: &HashMap<&'a str, Vec<&'a Edge>>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Vec<&'a Node> {
    let mut related = Vec::new();
    let mut related_ids = HashSet::<String>::new();
    let mut frontier = vec![seed_id];
    let mut visited = HashSet::<String>::new();

    while let Some(current_target) = frontier.pop() {
        if !visited.insert(current_target.to_string()) {
            continue;
        }
        let Some(edges) = edges_by_target.get(current_target) else {
            continue;
        };
        for edge in edges {
            match edge.kind {
                EdgeKind::Implements => frontier.push(edge.source.as_str()),
                EdgeKind::Calls | EdgeKind::Uses | EdgeKind::Reads | EdgeKind::TypeRef => {
                    let Some(node) = node_index.get(edge.source.as_str()).copied() else {
                        continue;
                    };
                    if related_ids.insert(node.id.clone()) {
                        related.push(node);
                    }
                }
                _ => {}
            }
        }
    }

    related
}

fn is_accessor_symbol(node: &Node) -> bool {
    node.kind == NodeKind::Function
        && (node.name.starts_with("getter:") || node.name.starts_with("setter:"))
}

fn should_skip_generated_container_scope(seed: &Node, scope: &Node) -> bool {
    if seed.file != scope.file {
        return false;
    }
    let file_name = seed
        .file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !(file_name.ends_with("Strings.generated.swift")
        || file_name.ends_with("Assets.generated.swift"))
    {
        return false;
    }

    matches!(
        scope.kind,
        NodeKind::Enum
            | NodeKind::Struct
            | NodeKind::Class
            | NodeKind::Extension
            | NodeKind::Module
    )
}

fn l10n_symbol_queries(record: &LocalizationCatalogRecord) -> Vec<String> {
    dedup_preserve_order(vec![
        record.key.clone(),
        snake_or_path_to_camel(&record.key),
        snake_or_path_to_pascal(&record.key),
    ])
}

fn asset_symbol_queries(record: &AssetRecord) -> Vec<String> {
    dedup_preserve_order(vec![
        record.name.clone(),
        snake_or_path_to_camel(&record.name),
        snake_or_path_to_pascal(&record.name),
        record
            .name
            .rsplit('/')
            .next()
            .map(snake_or_path_to_camel)
            .unwrap_or_default(),
    ])
}

fn dedup_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        let normalized = normalize_match_text(&value);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        deduped.push(value);
    }
    deduped
}

fn snake_or_path_to_camel(value: &str) -> String {
    let mut output = String::new();
    let mut upper_next = false;
    for ch in value.chars() {
        if !ch.is_alphanumeric() {
            upper_next = true;
            continue;
        }
        if output.is_empty() {
            output.extend(ch.to_lowercase());
            upper_next = false;
            continue;
        }
        if upper_next {
            output.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            output.extend(ch.to_lowercase());
        }
    }
    output
}

fn snake_or_path_to_pascal(value: &str) -> String {
    let camel = snake_or_path_to_camel(value);
    let mut chars = camel.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut output = String::new();
    output.extend(first.to_uppercase());
    output.extend(chars);
    output
}

fn normalize_match_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut normalized = String::new();
    let mut previous: Option<char> = None;
    let mut last_was_space = false;

    for ch in trimmed.chars() {
        if !ch.is_alphanumeric() {
            if !last_was_space && !normalized.is_empty() {
                normalized.push(' ');
                last_was_space = true;
            }
            previous = None;
            continue;
        }

        let starts_new_token = previous.is_some_and(|prev| {
            (prev.is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (prev.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (prev.is_ascii_digit() && ch.is_ascii_alphabetic())
        });
        if starts_new_token && !last_was_space && !normalized.is_empty() {
            normalized.push(' ');
        }

        for lower in ch.to_lowercase() {
            normalized.push(lower);
        }
        last_was_space = false;
        previous = Some(ch);
    }

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_concept(value: &str) -> String {
    normalize_match_text(value)
}

fn fuzzy_candidate_rank(query: &str, candidate: &str) -> Option<(usize, usize)> {
    if query.is_empty() || candidate.is_empty() || query == candidate {
        return None;
    }

    let query_len = query.chars().count();
    let candidate_len = candidate.chars().count();

    if query_len >= 4 && candidate.contains(query) {
        return Some((0, candidate_len.saturating_sub(query_len)));
    }
    if candidate_len >= 4 && query.contains(candidate) {
        return Some((0, query_len.saturating_sub(candidate_len)));
    }

    let max_distance = fuzzy_phrase_distance(query_len.max(candidate_len));
    let distance = levenshtein_bounded(query, candidate, max_distance)?;
    Some((distance, query_len.abs_diff(candidate_len)))
}

fn fuzzy_matches_text(normalized_value: &str, normalized_query: &str) -> bool {
    if normalized_value.is_empty()
        || normalized_query.is_empty()
        || normalized_value == normalized_query
        || normalized_value.contains(normalized_query)
    {
        return false;
    }

    if fuzzy_candidate_rank(normalized_query, normalized_value).is_some() {
        return true;
    }

    fuzzy_query_tokens_match(normalized_query, normalized_value)
}

fn fuzzy_query_tokens_match(normalized_query: &str, normalized_value: &str) -> bool {
    let value_tokens: Vec<&str> = normalized_value.split_whitespace().collect();
    let query_tokens: Vec<&str> = normalized_query.split_whitespace().collect();
    if value_tokens.is_empty() || query_tokens.is_empty() {
        return false;
    }

    query_tokens.into_iter().all(|query_token| {
        value_tokens.iter().any(|value_token| {
            if query_token == *value_token {
                return true;
            }
            let max_distance =
                fuzzy_token_distance(query_token.chars().count().max(value_token.chars().count()));
            levenshtein_bounded(query_token, value_token, max_distance).is_some()
        })
    })
}

fn fuzzy_phrase_distance(len: usize) -> usize {
    if len <= 4 {
        1
    } else if len <= 12 {
        2
    } else {
        3
    }
}

fn fuzzy_token_distance(len: usize) -> usize {
    if len <= 3 {
        0
    } else if len <= 6 {
        1
    } else {
        2
    }
}

fn levenshtein_bounded(left: &str, right: &str, max_distance: usize) -> Option<usize> {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    if left_chars.len().abs_diff(right_chars.len()) > max_distance {
        return None;
    }

    let mut previous: Vec<usize> = (0..=right_chars.len()).collect();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_index, left_char) in left_chars.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
            row_min = row_min.min(current[right_index + 1]);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    let distance = previous[right_chars.len()];
    (distance <= max_distance).then_some(distance)
}

fn match_kind_label(match_type: TextMatch) -> &'static str {
    match match_type {
        TextMatch::Exact => "exact",
        TextMatch::Contains => "contains",
        TextMatch::Fuzzy => "fuzzy",
    }
}

fn merge_evidence(target: &mut Vec<ConceptEvidence>, incoming: &[ConceptEvidence]) {
    let mut seen: HashSet<ConceptEvidence> = target.iter().cloned().collect();
    for evidence in incoming {
        if seen.insert(evidence.clone()) {
            target.push(evidence.clone());
        }
    }
}

fn sort_concepts(records: &mut [ConceptRecord]) {
    for record in records.iter_mut() {
        record.aliases.sort();
        record
            .aliases
            .dedup_by(|left, right| normalize_concept(left) == normalize_concept(right));
        record
            .bindings
            .sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
        record
            .bindings
            .dedup_by(|left, right| left.symbol_id == right.symbol_id);
    }

    records.sort_by(|left, right| left.concept.cmp(&right.concept));
}

fn snapshot_path(store_dir: &Path) -> PathBuf {
    store_dir.join(CONCEPTS_SNAPSHOT_FILE)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use tempfile::tempdir;

    use grapha_core::graph::{Edge, Graph, Node, NodeKind, Span, Visibility};

    use super::*;

    fn make_node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 1],
                end: [1, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Gift".to_string()),
            snippet: None,
            repo: None,
        }
    }

    fn build_search_index(graph: &Graph) -> (tempfile::TempDir, Index) {
        let dir = tempdir().unwrap();
        let index = search::build_index(graph, &dir.path().join("search_index")).unwrap();
        (dir, index)
    }

    #[test]
    fn concept_index_loads_missing_store_as_empty() {
        let dir = tempdir().unwrap();
        let index = load_concept_index_from_store(dir.path()).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn concept_index_persists_bindings_and_aliases() {
        let dir = tempdir().unwrap();
        let mut index = ConceptIndex::default();
        index
            .bind_concept(
                "送礼横幅",
                &[String::from("gift-banner-page")],
                vec![ConceptEvidence {
                    kind: "manual".to_string(),
                    value: "送礼横幅".to_string(),
                    match_kind: "confirmed".to_string(),
                    table: None,
                    key: None,
                    source_value: None,
                    ui_path: Vec::new(),
                    note: Some("manual".to_string()),
                }],
            )
            .unwrap();
        index
            .add_aliases("送礼横幅", &[String::from("礼物 banner")])
            .unwrap();
        save_concept_index_to_store(dir.path(), &index).unwrap();

        let loaded = load_concept_index_from_store(dir.path()).unwrap();
        let (record, lookup) = loaded.record_for_term("礼物 banner").unwrap();
        assert_eq!(record.concept, "送礼横幅");
        assert_eq!(lookup.match_kind, "alias");
        assert_eq!(record.bindings.len(), 1);
    }

    #[test]
    fn search_concepts_prefers_confirmed_binding_over_heuristics() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![make_node(
                "gift-banner-page",
                "GiftBannerPage",
                NodeKind::Struct,
                "GiftBannerPage.swift",
            )],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);
        let mut concepts = ConceptIndex::default();
        concepts
            .bind_concept(
                "送礼横幅",
                &[String::from("gift-banner-page")],
                vec![ConceptEvidence {
                    kind: "manual".to_string(),
                    value: "送礼横幅".to_string(),
                    match_kind: "confirmed".to_string(),
                    table: None,
                    key: None,
                    source_value: None,
                    ui_path: Vec::new(),
                    note: Some("seed".to_string()),
                }],
            )
            .unwrap();

        let result = search_concepts(
            &graph,
            &search_index,
            &concepts,
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "送礼横幅",
            5,
        )
        .unwrap();

        assert_eq!(result.resolved_from, "concept_store");
        assert_eq!(result.scopes.len(), 1);
        assert_eq!(result.scopes[0].symbol.id, "gift-banner-page");
        assert_eq!(result.scopes[0].status, STATUS_CONFIRMED);
    }

    #[test]
    fn search_concepts_fuzzy_matches_stored_concept_by_default() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![make_node(
                "gift-banner-page",
                "GiftBannerPage",
                NodeKind::Struct,
                "GiftBannerPage.swift",
            )],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);
        let mut concepts = ConceptIndex::default();
        concepts
            .bind_concept(
                "gift banner",
                &[String::from("gift-banner-page")],
                Vec::new(),
            )
            .unwrap();

        let result = search_concepts(
            &graph,
            &search_index,
            &concepts,
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "gift baner",
            5,
        )
        .unwrap();

        assert_eq!(result.resolved_from, "concept_store");
        assert_eq!(result.matched_concept.as_deref(), Some("gift banner"));
        assert_eq!(result.scopes[0].symbol.id, "gift-banner-page");
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.match_kind == "fuzzy_concept")
        );
    }

    #[test]
    fn search_concepts_fuzzy_matches_localized_values_by_default() {
        let owner = make_node(
            "gift-banner-page",
            "GiftBannerPage",
            NodeKind::Struct,
            "GiftBannerPage.swift",
        );
        let mut usage = make_node(
            "gift-banner-title",
            "bannerTitle",
            NodeKind::Property,
            "GiftBannerPage.swift",
        );
        usage
            .metadata
            .insert("l10n.ref_kind".to_string(), "literal".to_string());
        usage
            .metadata
            .insert("l10n.literal".to_string(), "gift_banner_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![owner.clone(), usage],
            edges: vec![Edge {
                source: owner.id.clone(),
                target: "gift-banner-title".to_string(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let (_dir, search_index) = build_search_index(&graph);
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "gift_banner_title".to_string(),
            catalog_file: "Resources/Localizable.xcstrings".to_string(),
            catalog_dir: "Resources".to_string(),
            source_language: "en".to_string(),
            source_value: "Gift banner".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &catalogs,
            &AssetCatalogIndex::default(),
            "gift baner",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, owner.id);
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "l10n_value" && evidence.match_kind == "fuzzy")
        );
    }

    #[test]
    fn search_concepts_survives_a_prose_query_and_still_matches_comments() {
        // Regression: a whole natural-language question used to blow tantivy's
        // regex state ceiling in the fuzzy ranking pass, and the `?` on that
        // pass failed the ENTIRE concept search — discarding the doc_comment /
        // snippet substring matches that had already succeeded. The comment
        // scan is the only arm that sees CJK at all (the BM25 tokenizer drops
        // Han characters at index time), so the failure was total for a CJK
        // question even though the evidence was sitting right there.
        let mut node = make_node(
            "danmaku-cell-model",
            "WYRoomDanmakuCellModel",
            NodeKind::Class,
            "Modules/WYRoom/Danmu/Model/WYRoomDanmakuCellModel.swift",
        );
        node.doc_comment =
            Some("// 计算展示时间，让弹幕在大约 5 到 8 秒内完全穿过屏幕".to_string());
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node],
            edges: Vec::<Edge>::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);

        let question = "wyak的房间弹幕样式，是用用户自己佩戴的气泡样式吗，还是统一的样式";
        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            question,
            5,
        )
        .expect("a prose question must not fail the whole concept search");

        // The question itself is not a verbatim substring of any comment, so it
        // legitimately finds nothing — the point is that it RETURNS instead of
        // erroring, leaving the model free to retry with a narrower term.
        assert!(result.scopes.is_empty());

        // ...and the narrower term the model should reach for still works.
        let narrowed = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "弹幕",
            5,
        )
        .unwrap();
        assert_eq!(narrowed.scopes.len(), 1);
        assert_eq!(narrowed.scopes[0].symbol.id, "danmaku-cell-model");
    }

    #[test]
    fn search_concepts_does_not_fuzzy_match_unrelated_short_cjk_values() {
        let banner_owner = make_node(
            "banner-page",
            "BannerPage",
            NodeKind::Struct,
            "BannerPage.swift",
        );
        let mut banner_usage = make_node(
            "banner-title",
            "bannerTitle",
            NodeKind::Property,
            "BannerPage.swift",
        );
        banner_usage
            .metadata
            .insert("l10n.ref_kind".to_string(), "literal".to_string());
        banner_usage
            .metadata
            .insert("l10n.literal".to_string(), "banner_title".to_string());

        let submit_owner = make_node(
            "submit-page",
            "SubmitPage",
            NodeKind::Struct,
            "SubmitPage.swift",
        );
        let mut submit_usage = make_node(
            "submit-title",
            "submitTitle",
            NodeKind::Property,
            "SubmitPage.swift",
        );
        submit_usage
            .metadata
            .insert("l10n.ref_kind".to_string(), "literal".to_string());
        submit_usage
            .metadata
            .insert("l10n.literal".to_string(), "submit_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                banner_owner.clone(),
                banner_usage,
                submit_owner.clone(),
                submit_usage,
            ],
            edges: vec![
                Edge {
                    source: banner_owner.id.clone(),
                    target: "banner-title".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: submit_owner.id.clone(),
                    target: "submit-title".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };
        let (_dir, search_index) = build_search_index(&graph);
        let catalogs = LocalizationCatalogIndex::from_records(vec![
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "banner_title".to_string(),
                catalog_file: "Resources/Localizable.xcstrings".to_string(),
                catalog_dir: "Resources".to_string(),
                source_language: "zh-Hans".to_string(),
                source_value: "首页横幅".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "submit_title".to_string(),
                catalog_file: "Resources/Localizable.xcstrings".to_string(),
                catalog_dir: "Resources".to_string(),
                source_language: "zh-Hans".to_string(),
                source_value: "提交".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
        ]);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &catalogs,
            &AssetCatalogIndex::default(),
            "横幅",
            5,
        )
        .unwrap();

        assert!(
            result
                .scopes
                .iter()
                .any(|scope| scope.symbol.id == banner_owner.id)
        );
        assert!(
            result
                .scopes
                .iter()
                .all(|scope| scope.symbol.id != submit_owner.id),
            "unrelated two-character CJK values should not be fuzzy matches"
        );
    }

    #[test]
    fn search_concepts_fuzzy_matches_symbols_by_default() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![make_node(
                "gift-banner-page",
                "GiftBannerPage",
                NodeKind::Struct,
                "GiftBannerPage.swift",
            )],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "gift baner",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, "gift-banner-page");
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "symbol_query")
        );
    }

    #[test]
    fn search_concepts_matches_symbol_doc_comments() {
        let mut node = make_node(
            "gift-coordinator",
            "Coordinator",
            NodeKind::Struct,
            "GiftCoordinator.swift",
        );
        node.doc_comment = Some("Coordinates the gift flow between catalog and checkout.".into());
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "gift flow",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, node.id);
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "doc_comment"
                    && evidence
                        .source_value
                        .as_deref()
                        .is_some_and(|value| value.contains("gift flow"))),
            "doc-only concept matches should report doc_comment evidence: {:?}",
            result.scopes[0].evidence
        );
    }

    #[test]
    fn search_concepts_matches_cjk_symbol_doc_comments() {
        let mut node = make_node(
            "activity-task-code.newUserRoomTask",
            "newUserRoomTask",
            NodeKind::Variant,
            "ActivityAPI.swift",
        );
        node.doc_comment = Some("/// 新用户房主任务?".into());
        node.snippet = Some("case newUserRoomTask = 11".into());
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "新用户房主任务",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, node.id);
        assert_eq!(
            result.scopes[0].symbol.doc_comment.as_deref(),
            Some("/// 新用户房主任务?")
        );
        assert_eq!(
            result.scopes[0].symbol.snippet.as_deref(),
            Some("case newUserRoomTask = 11")
        );
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "doc_comment"
                    && matches!(evidence.match_kind.as_str(), "exact" | "contains")
                    && evidence
                        .source_value
                        .as_deref()
                        .is_some_and(|value| value.contains("新用户房主任务"))),
            "CJK doc comment concept matches should report doc_comment evidence: {:?}",
            result.scopes[0].evidence
        );
    }

    #[test]
    fn search_concepts_matches_symbol_snippets_and_compacts_output() {
        let mut node = make_node(
            "room-title",
            "titleStr",
            NodeKind::Property,
            "MyRoomTaskView.swift",
        );
        node.snippet = Some(
            r#"
            private var titleStr: String {
                if viewModel.activityType == .newUser {
                    "新用户房主任务"
                } else {
                    "每日任务"
                }
            }
            "#
            .into(),
        );
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "新用户房主任务",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, node.id);
        let snippet = result.scopes[0].symbol.snippet.as_deref().unwrap();
        assert!(
            !snippet.contains('\n'),
            "snippet should be compact: {snippet}"
        );
        assert!(snippet.starts_with("private var titleStr: String {"));
        assert!(
            result.scopes[0].evidence.iter().any(|evidence| {
                evidence.kind == "snippet"
                    && evidence.source_value.as_deref().is_some_and(|value| {
                        !value.contains('\n') && value.contains("新用户房主任务")
                    })
            }),
            "snippet concept matches should report compact snippet evidence: {:?}",
            result.scopes[0].evidence
        );
    }

    #[test]
    fn project_concept_search_result_hides_file_and_span_when_unselected() {
        let symbol = SymbolInfo {
            id: "room-title".to_string(),
            locator: Some("Room::Task.swift::titleStr".to_string()),
            name: "titleStr".to_string(),
            kind: NodeKind::Property,
            file: "Task.swift".to_string(),
            span: [42, 42],
            visibility: Some(Visibility::Public),
            role: None,
            signature: None,
            doc_comment: Some("Task title".to_string()),
            annotation: None,
            module: Some("Room".to_string()),
            snippet: Some("var titleStr: String".to_string()),
            repo: Some("lama-ludo-ios".to_string()),
        };
        let result = ConceptSearchResult {
            query: "task title".to_string(),
            resolved_from: "heuristics".to_string(),
            matched_concept: None,
            scopes: vec![ConceptScopeMatch {
                symbol,
                score: 1.0,
                status: STATUS_CANDIDATE.to_string(),
                evidence: Vec::new(),
            }],
        };

        let projected = project_concept_search_result(
            &result,
            crate::fields::FieldSet::all().without_file().without_span(),
        );
        let payload = serde_json::to_value(&projected).unwrap();
        let symbol = &payload["scopes"][0]["symbol"];

        assert!(symbol.get("file").is_none());
        assert!(symbol.get("span").is_none());
        assert_eq!(symbol["id"], "room-title");
        assert_eq!(symbol["snippet"], "var titleStr: String");
    }

    fn new_room_projection_result() -> ConceptSearchResult {
        let symbol = SymbolInfo {
            id: "room-entrance.newRoom".to_string(),
            locator: Some("Room::Task.swift::newRoom".to_string()),
            name: "newRoom".to_string(),
            kind: NodeKind::Variant,
            file: "Task.swift".to_string(),
            span: [42, 42],
            visibility: Some(Visibility::Public),
            role: None,
            signature: None,
            doc_comment: Some("//新房主任务".to_string()),
            annotation: None,
            module: Some("Room".to_string()),
            snippet: Some("case newRoom = 1  //新房主任务".to_string()),
            repo: Some("lama-ludo-ios".to_string()),
        };
        ConceptSearchResult {
            query: "新用户房主任务".to_string(),
            resolved_from: "heuristics".to_string(),
            matched_concept: None,
            scopes: vec![ConceptScopeMatch {
                symbol,
                score: 575.0,
                status: STATUS_CANDIDATE.to_string(),
                evidence: vec![
                    ConceptEvidence {
                        kind: "doc_comment".to_string(),
                        value: "新用户房主任务".to_string(),
                        match_kind: "fuzzy".to_string(),
                        table: None,
                        key: None,
                        source_value: Some("//新房主任务".to_string()),
                        ui_path: Vec::new(),
                        note: Some("newRoom".to_string()),
                    },
                    ConceptEvidence {
                        kind: "snippet".to_string(),
                        value: "新用户房主任务".to_string(),
                        match_kind: "fuzzy".to_string(),
                        table: None,
                        key: None,
                        source_value: Some("case newRoom = 1  //新房主任务".to_string()),
                        ui_path: Vec::new(),
                        note: Some("newRoom".to_string()),
                    },
                ],
            }],
        }
    }

    #[test]
    fn project_concept_search_result_uses_concise_evidence_when_not_full() {
        let result = new_room_projection_result();
        let mut fields = FieldSet::none();
        fields.id = true;
        fields.locator = true;
        fields.module = true;
        fields.repo = true;
        fields.visibility = true;

        let projected = project_concept_search_result(&result, fields);
        let payload = serde_json::to_value(&projected).unwrap();
        let symbol = &payload["scopes"][0]["symbol"];
        let evidence = payload["scopes"][0]["evidence"].as_array().unwrap();

        assert!(symbol.get("doc_comment").is_none());
        assert!(symbol.get("snippet").is_none());
        assert_eq!(symbol["id"], "room-entrance.newRoom");
        assert_eq!(symbol["locator"], "Room::Task.swift::newRoom");
        assert_eq!(symbol["module"], "Room");
        assert_eq!(symbol["repo"], "lama-ludo-ios");
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0]["kind"], "doc_comment");
        assert_eq!(evidence[0]["match_kind"], "fuzzy");
        assert_eq!(evidence[0]["source_value"], "//新房主任务");
        assert!(evidence[0].get("value").is_none());
        assert!(evidence[0].get("matched_query_term").is_none());
        assert!(evidence[0].get("note").is_none());
    }

    #[test]
    fn project_concept_search_result_keeps_full_evidence_for_full_fields() {
        let result = new_room_projection_result();

        let projected = project_concept_search_result(&result, FieldSet::all());
        let payload = serde_json::to_value(&projected).unwrap();
        let symbol = &payload["scopes"][0]["symbol"];
        let evidence = payload["scopes"][0]["evidence"].as_array().unwrap();

        assert_eq!(symbol["doc_comment"], "//新房主任务");
        assert_eq!(symbol["snippet"], "case newRoom = 1  //新房主任务");
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0]["value"], "新用户房主任务");
        assert!(evidence[0].get("matched_query_term").is_none());
        assert_eq!(evidence[0]["note"], "newRoom");
        assert_eq!(evidence[1]["kind"], "snippet");
    }

    #[test]
    fn search_concepts_matches_symbol_annotations() {
        let node = make_node(
            "gift-coordinator",
            "Coordinator",
            NodeKind::Struct,
            "GiftCoordinator.swift",
        );
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);
        let store_dir = tempdir().unwrap();
        let annotation_store = crate::annotations::AnnotationStore::for_store_dir(store_dir.path());
        annotation_store
            .upsert_for_node(
                &node,
                "Coordinates the gift handoff between catalog and checkout.",
                Some("codex"),
            )
            .unwrap();
        let annotations = annotation_store.load_index().unwrap();

        let result = search_concepts_with_annotations(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "gift handoff",
            5,
            Some(&annotations),
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, node.id);
        assert_eq!(
            result.scopes[0]
                .symbol
                .annotation
                .as_ref()
                .map(|annotation| annotation.text.as_str()),
            Some("Coordinates the gift handoff between catalog and checkout.")
        );
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "annotation"
                    && evidence
                        .source_value
                        .as_deref()
                        .is_some_and(|value| value.contains("gift handoff"))),
            "annotation-only concept matches should report annotation evidence: {:?}",
            result.scopes[0].evidence
        );

        let fuzzy_result = search_concepts_with_annotations(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &AssetCatalogIndex::default(),
            "gft hndoff",
            5,
            Some(&annotations),
        )
        .unwrap();

        assert!(
            fuzzy_result.scopes[0].evidence.iter().any(|evidence| {
                evidence.kind == "annotation" && evidence.match_kind == "fuzzy"
            }),
            "fuzzy annotation concept matches should report fuzzy annotation evidence: {:?}",
            fuzzy_result.scopes[0].evidence
        );
    }

    #[test]
    fn search_concepts_resolves_localized_value_to_owner_scope() {
        let owner = make_node(
            "gift-banner-page",
            "GiftBannerPage",
            NodeKind::Struct,
            "GiftBannerPage.swift",
        );
        let mut usage = make_node(
            "gift-banner-title",
            "bannerTitle",
            NodeKind::Property,
            "GiftBannerPage.swift",
        );
        usage
            .metadata
            .insert("l10n.ref_kind".to_string(), "literal".to_string());
        usage
            .metadata
            .insert("l10n.literal".to_string(), "gift_banner_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![owner.clone(), usage],
            edges: vec![Edge {
                source: owner.id.clone(),
                target: "gift-banner-title".to_string(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let (_dir, search_index) = build_search_index(&graph);
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "gift_banner_title".to_string(),
            catalog_file: "Resources/Localizable.xcstrings".to_string(),
            catalog_dir: "Resources".to_string(),
            source_language: "zh-Hans".to_string(),
            source_value: "送礼横幅".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &catalogs,
            &AssetCatalogIndex::default(),
            "送礼横幅",
            5,
        )
        .unwrap();

        assert_eq!(result.resolved_from, "heuristics");
        assert_eq!(result.scopes[0].symbol.id, owner.id);
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "l10n_value")
        );
    }

    #[test]
    fn search_concepts_resolves_asset_usage_to_owner_scope() {
        let owner = make_node(
            "gift-banner-page",
            "GiftBannerPage",
            NodeKind::Struct,
            "GiftBannerPage.swift",
        );
        let mut asset_usage = make_node(
            "gift-banner-icon",
            "giftIcon",
            NodeKind::Property,
            "GiftBannerPage.swift",
        );
        asset_usage
            .metadata
            .insert("asset.ref_kind".to_string(), "image".to_string());
        asset_usage
            .metadata
            .insert("asset.name".to_string(), "gift/banner".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![owner.clone(), asset_usage],
            edges: vec![Edge {
                source: owner.id.clone(),
                target: "gift-banner-icon".to_string(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let (_dir, search_index) = build_search_index(&graph);
        let assets_index = AssetCatalogIndex::from_records(vec![AssetRecord {
            name: "gift/banner".to_string(),
            group_path: "gift".to_string(),
            catalog: "Assets".to_string(),
            catalog_dir: "Resources".to_string(),
            template_intent: None,
            provides_namespace: None,
        }]);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &assets_index,
            "gift/banner",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, owner.id);
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "asset_name")
        );
    }

    #[test]
    fn search_concepts_falls_back_to_l10n_wrapper_when_record_has_no_usage_sites() {
        let wrapper = make_node(
            "l10n-gift-record",
            "taskHelpTabGiftRecord",
            NodeKind::Property,
            "Strings.generated.swift",
        );
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![wrapper.clone()],
            edges: Vec::new(),
        };
        let (_dir, search_index) = build_search_index(&graph);
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "task_help_tab_gift_record".to_string(),
            catalog_file: "Resources/Localizable.xcstrings".to_string(),
            catalog_dir: "Resources".to_string(),
            source_language: "zh-Hans".to_string(),
            source_value: "Gift records".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::from([(String::from("zh-Hans"), String::from("送礼记录"))]),
        }]);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &catalogs,
            &AssetCatalogIndex::default(),
            "送礼记录",
            5,
        )
        .unwrap();

        assert_eq!(result.resolved_from, "heuristics");
        assert_eq!(result.scopes[0].symbol.id, wrapper.id);
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "l10n_wrapper")
        );
    }

    #[test]
    fn search_concepts_matches_asset_tokens_and_lifts_to_caller_scope() {
        let owner = make_node(
            "gift-banner-view",
            "GiftNotifyBannerView",
            NodeKind::Struct,
            "GiftNotifyBannerView.swift",
        );
        let caller = make_node(
            "gift-banner-image",
            "bannerImage",
            NodeKind::Property,
            "GiftNotifyBannerView.swift",
        );
        let asset = make_node(
            "room-gift-banner-1",
            "roomGiftBanner1",
            NodeKind::Property,
            "Assets.generated.swift",
        );

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![owner.clone(), caller.clone(), asset],
            edges: vec![
                Edge {
                    source: owner.id.clone(),
                    target: caller.id.clone(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: caller.id.clone(),
                    target: "room-gift-banner-1".to_string(),
                    kind: EdgeKind::Calls,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };
        let (_dir, search_index) = build_search_index(&graph);
        let assets_index = AssetCatalogIndex::from_records(vec![AssetRecord {
            name: "room_gift_banner_1".to_string(),
            group_path: "Room".to_string(),
            catalog: "Assets".to_string(),
            catalog_dir: "Resources".to_string(),
            template_intent: None,
            provides_namespace: None,
        }]);

        let result = search_concepts(
            &graph,
            &search_index,
            &ConceptIndex::default(),
            &LocalizationCatalogIndex::default(),
            &assets_index,
            "gift banner",
            5,
        )
        .unwrap();

        assert_eq!(result.scopes[0].symbol.id, owner.id);
        assert!(
            result.scopes[0]
                .evidence
                .iter()
                .any(|evidence| evidence.kind == "asset_wrapper")
        );
    }

    #[test]
    fn prune_removes_stale_bindings_without_dropping_record() {
        let mut index = ConceptIndex::from_records(vec![ConceptRecord {
            concept: "送礼横幅".to_string(),
            aliases: Vec::new(),
            bindings: vec![ConceptBinding {
                symbol_id: "stale-id".to_string(),
                status: STATUS_CONFIRMED.to_string(),
                evidence: Vec::new(),
            }],
            notes: None,
        }]);
        let valid_ids: HashSet<&str> = HashSet::new();

        let result = index.prune(&valid_ids);

        assert_eq!(result.pruned_bindings, 1);
        let (record, _) = index.record_for_term("送礼横幅").unwrap();
        assert!(record.bindings.is_empty());
    }
}
