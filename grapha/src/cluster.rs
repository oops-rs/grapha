use std::collections::BTreeMap;

use grapha_core::graph::{EdgeKind, Graph};
use serde::Serialize;
use serde_json::{Value, json};

use crate::changes::ChangeReport;
use crate::concepts::{ConceptSearchResult, ProjectedConceptSearchResult};
use crate::inferred::InferredBuildResult;
use crate::query::origin::OriginResult;
use crate::query::{
    self, ContextResult, arch::ArchitectureResult, entries::EntriesResult,
    file_symbols::FileSymbolsResult, impact::ImpactResult, reverse::ReverseResult,
    smells::SmellsResult, trace::TraceResult, usages::UsagesResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreBand {
    Excellent,
    Strong,
    Possible,
    Weak,
    Unknown,
}

impl ScoreBand {
    const ORDER: [Self; 5] = [
        Self::Excellent,
        Self::Strong,
        Self::Possible,
        Self::Weak,
        Self::Unknown,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Excellent => "excellent",
            Self::Strong => "strong",
            Self::Possible => "possible",
            Self::Weak => "weak",
            Self::Unknown => "unknown",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Excellent => "Excellent",
            Self::Strong => "Strong",
            Self::Possible => "Possible",
            Self::Weak => "Weak",
            Self::Unknown => "Unknown",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "excellent" => Some(Self::Excellent),
            "strong" => Some(Self::Strong),
            "possible" => Some(Self::Possible),
            "weak" => Some(Self::Weak),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    fn for_normalized(score: Option<f64>) -> Self {
        match score {
            Some(score) if score >= 0.85 => Self::Excellent,
            Some(score) if score >= 0.65 => Self::Strong,
            Some(score) if score >= 0.40 => Self::Possible,
            Some(_) => Self::Weak,
            None => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResultScore {
    pub normalized: Option<f64>,
    pub source: String,
    pub native: bool,
}

impl ResultScore {
    pub fn native(value: f64, source: impl Into<String>) -> Self {
        Self::new(value, source, true)
    }

    pub fn fallback(value: f64, source: impl Into<String>) -> Self {
        Self::new(value, source, false)
    }

    pub fn unknown(source: impl Into<String>) -> Self {
        Self {
            normalized: None,
            source: source.into(),
            native: false,
        }
    }

    fn new(value: f64, source: impl Into<String>, native: bool) -> Self {
        let normalized = value.is_finite().then(|| value.clamp(0.0, 1.0));
        Self {
            normalized,
            source: source.into(),
            native,
        }
    }

    pub fn band(&self) -> ScoreBand {
        ScoreBand::for_normalized(self.normalized)
    }
}

#[derive(Debug, Clone)]
pub struct ClusterOptions {
    pub cluster_id: Option<String>,
    pub page: usize,
    pub per_page: usize,
    pub candidate_limit: usize,
}

impl ClusterOptions {
    pub fn new(
        cluster_id: Option<String>,
        page: usize,
        per_page: usize,
        candidate_limit: usize,
    ) -> Self {
        Self {
            cluster_id,
            page: page.max(1),
            per_page: per_page.max(1),
            candidate_limit,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClusterSummary {
    pub id: String,
    pub band: ScoreBand,
    pub label: String,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_max: Option<f64>,
    pub native_scores: usize,
    pub fallback_scores: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClusterPage<T> {
    pub cluster_id: String,
    pub band: ScoreBand,
    pub page: usize,
    pub per_page: usize,
    pub total_items: usize,
    pub total_pages: usize,
    pub returned: usize,
    pub candidate_limit: usize,
    pub items: Vec<ClusteredItem<T>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClusteredItem<T> {
    pub section: String,
    pub score: ResultScore,
    pub item: T,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClusteredResult<T> {
    pub result: Value,
    pub clusters: Vec<ClusterSummary>,
    pub page: ClusterPage<T>,
}

#[derive(Debug, Clone)]
pub struct ClusterInput<T> {
    pub section: String,
    pub score: ResultScore,
    pub item: T,
    original_index: usize,
}

impl<T> ClusterInput<T> {
    pub fn new(
        section: impl Into<String>,
        score: ResultScore,
        item: T,
        original_index: usize,
    ) -> Self {
        Self {
            section: section.into(),
            score,
            item,
            original_index,
        }
    }
}

pub fn normalize_bm25_scores(scores: &[f32]) -> Vec<ResultScore> {
    let max_score = scores
        .iter()
        .copied()
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0_f32, f32::max);

    scores
        .iter()
        .map(|score| {
            if max_score > 0.0 && score.is_finite() {
                ResultScore::native((*score / max_score) as f64, "bm25")
            } else {
                ResultScore::unknown("bm25")
            }
        })
        .collect()
}

pub fn fallback_rank_score(index: usize, total: usize, source: impl Into<String>) -> ResultScore {
    if total == 0 {
        return ResultScore::unknown(source);
    }
    let position = if total <= 1 {
        0.0
    } else {
        index as f64 / (total - 1) as f64
    };
    // Unscored list sections should be navigable without pretending to have
    // strong relevance evidence. Keep them in possible/weak bands by rank.
    ResultScore::fallback(0.55 - (0.20 * position), source)
}

pub fn clustered_result<T>(
    result: Value,
    items: Vec<ClusterInput<T>>,
    options: &ClusterOptions,
) -> ClusteredResult<T> {
    let mut grouped: BTreeMap<ScoreBand, Vec<ClusterInput<T>>> = BTreeMap::new();
    for item in items {
        grouped.entry(item.score.band()).or_default().push(item);
    }

    for items in grouped.values_mut() {
        if items.iter().any(|item| item.score.native) {
            items.sort_by(|left, right| {
                right
                    .score
                    .normalized
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&left.score.normalized.unwrap_or(f64::NEG_INFINITY))
                    .then_with(|| left.original_index.cmp(&right.original_index))
            });
        } else {
            items.sort_by_key(|item| item.original_index);
        }
    }

    let clusters: Vec<ClusterSummary> = ScoreBand::ORDER
        .iter()
        .filter_map(|band| {
            let items = grouped.get(band)?;
            let mut score_min: Option<f64> = None;
            let mut score_max: Option<f64> = None;
            let mut native_scores = 0usize;
            let mut fallback_scores = 0usize;

            for item in items {
                if item.score.native {
                    native_scores += 1;
                } else if item.score.normalized.is_some() {
                    fallback_scores += 1;
                }
                if let Some(score) = item.score.normalized {
                    score_min = Some(score_min.map_or(score, |current| current.min(score)));
                    score_max = Some(score_max.map_or(score, |current| current.max(score)));
                }
            }

            Some(ClusterSummary {
                id: band.id().to_string(),
                band: *band,
                label: band.label().to_string(),
                total: items.len(),
                score_min,
                score_max,
                native_scores,
                fallback_scores,
            })
        })
        .collect();

    let selected_band = options
        .cluster_id
        .as_deref()
        .and_then(ScoreBand::from_id)
        .or_else(|| clusters.first().map(|cluster| cluster.band))
        .unwrap_or(ScoreBand::Unknown);

    let selected_items = grouped.remove(&selected_band).unwrap_or_default();
    let total_items = selected_items.len();
    let total_pages = total_items.div_ceil(options.per_page);
    let start = options
        .page
        .saturating_sub(1)
        .saturating_mul(options.per_page);
    let page_items = selected_items
        .into_iter()
        .skip(start)
        .take(options.per_page)
        .map(|item| ClusteredItem {
            section: item.section,
            score: item.score,
            item: item.item,
        })
        .collect::<Vec<_>>();

    ClusteredResult {
        result,
        clusters,
        page: ClusterPage {
            cluster_id: selected_band.id().to_string(),
            band: selected_band,
            page: options.page,
            per_page: options.per_page,
            total_items,
            total_pages,
            returned: page_items.len(),
            candidate_limit: options.candidate_limit,
            items: page_items,
        },
    }
}

pub fn clustered_value(
    result: Value,
    items: Vec<ClusterInput<Value>>,
    options: &ClusterOptions,
) -> Value {
    serde_json::to_value(clustered_result(result, items, options)).unwrap_or(Value::Null)
}

pub fn search_items<T: Serialize>(
    result: Value,
    items: &[T],
    raw_scores: &[f32],
    options: &ClusterOptions,
) -> Value {
    let scores = normalize_bm25_scores(raw_scores);
    let inputs = items
        .iter()
        .zip(scores)
        .enumerate()
        .map(|(index, (item, score))| {
            ClusterInput::new(
                "results",
                score,
                serde_json::to_value(item).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();
    clustered_value(result, inputs, options)
}

pub fn concept_search_result(
    result: &ProjectedConceptSearchResult,
    options: &ClusterOptions,
) -> Value {
    concept_search_value(
        &result.query,
        &result.resolved_from,
        &result.matched_concept,
        &result.scopes,
        result.scopes.iter().map(|scope| scope.score),
        options,
    )
}

pub fn concept_search_full_result(result: &ConceptSearchResult, options: &ClusterOptions) -> Value {
    concept_search_value(
        &result.query,
        &result.resolved_from,
        &result.matched_concept,
        &result.scopes,
        result.scopes.iter().map(|scope| scope.score),
        options,
    )
}

pub fn context_result(result: &ContextResult, graph: &Graph, options: &ClusterOptions) -> Value {
    let mut items = Vec::new();
    let source_id = result.symbol.id.as_str();
    push_symbol_refs(&mut items, "callers", &result.callers, |item| {
        edge_score(graph, item.id.as_str(), source_id, EdgeKind::Calls)
    });
    push_symbol_refs(&mut items, "callees", &result.callees, |item| {
        edge_score(graph, source_id, item.id.as_str(), EdgeKind::Calls)
    });
    push_symbol_refs(&mut items, "reads", &result.reads, |item| {
        edge_score(graph, source_id, item.id.as_str(), EdgeKind::Reads)
    });
    push_symbol_refs(&mut items, "read_by", &result.read_by, |item| {
        edge_score(graph, item.id.as_str(), source_id, EdgeKind::Reads)
    });
    push_symbol_refs(
        &mut items,
        "invalidation_sources",
        &result.invalidation_sources,
        |_| None,
    );
    push_symbol_refs(&mut items, "contains", &result.contains, |item| {
        edge_score(graph, source_id, item.id.as_str(), EdgeKind::Contains)
    });
    push_symbol_refs(&mut items, "contained_by", &result.contained_by, |item| {
        edge_score(graph, item.id.as_str(), source_id, EdgeKind::Contains)
    });
    push_symbol_refs(&mut items, "implementors", &result.implementors, |item| {
        edge_score(graph, item.id.as_str(), source_id, EdgeKind::Implements)
    });
    push_symbol_refs(&mut items, "implements", &result.implements, |item| {
        edge_score(graph, source_id, item.id.as_str(), EdgeKind::Implements)
    });
    push_symbol_refs(&mut items, "type_refs", &result.type_refs, |item| {
        edge_score(graph, source_id, item.id.as_str(), EdgeKind::TypeRef)
    });

    clustered_value(
        json!({
            "symbol": result.symbol,
            "total_callers": result.total_callers,
            "total_callees": result.total_callees,
            "total_reads": result.total_reads,
            "total_read_by": result.total_read_by,
            "total_invalidation_sources": result.total_invalidation_sources,
            "total_contains": result.total_contains,
            "contains_tree": result.contains_tree,
            "total_contained_by": result.total_contained_by,
            "total_implementors": result.total_implementors,
            "total_implements": result.total_implements,
            "total_type_refs": result.total_type_refs,
        }),
        items,
        options,
    )
}

pub fn impact_result(result: &ImpactResult, options: &ClusterOptions) -> Value {
    let mut items = Vec::new();
    push_scored_values(
        &mut items,
        "depth_1",
        &result.depth_1,
        ResultScore::native(1.0, "reachability_depth"),
    );
    push_scored_values(
        &mut items,
        "depth_2",
        &result.depth_2,
        ResultScore::native(0.75, "reachability_depth"),
    );
    push_scored_values(
        &mut items,
        "depth_3_plus",
        &result.depth_3_plus,
        ResultScore::native(0.50, "reachability_depth"),
    );

    clustered_value(
        json!({
            "source": result.source,
            "summary": result.summary,
            "total_depth_1": result.total_depth_1,
            "total_depth_2": result.total_depth_2,
            "total_depth_3_plus": result.total_depth_3_plus,
            "total_affected": result.total_affected,
        }),
        items,
        options,
    )
}

pub fn trace_result(result: &TraceResult, options: &ClusterOptions) -> Value {
    let total = result.flows.len();
    let items = result
        .flows
        .iter()
        .enumerate()
        .map(|(index, flow)| {
            let hops = flow.path.len().saturating_sub(1) as f64;
            let terminal_bonus = if flow.terminal.is_some() { 0.10 } else { 0.0 };
            let score = (0.90 + terminal_bonus - (hops * 0.05)).clamp(0.35, 1.0);
            ClusterInput::new(
                "flows",
                ResultScore::native(score, "path_confidence"),
                serde_json::to_value(flow).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "entry": result.entry,
            "requested_symbol": result.requested_symbol,
            "traced_roots": result.traced_roots,
            "fallback_used": result.fallback_used,
            "hint": result.hint,
            "total_flows": result.total_flows,
            "summary": result.summary,
            "candidate_flows": total,
        }),
        items,
        options,
    )
}

pub fn reverse_result(result: &ReverseResult, options: &ClusterOptions) -> Value {
    let items = result
        .affected_entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let score = 1.0 / (entry.distance as f64 + 1.0);
            ClusterInput::new(
                "affected_entries",
                ResultScore::native(score, "reachability_distance"),
                serde_json::to_value(entry).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "symbol": result.symbol,
            "total_entries": result.total_entries,
        }),
        items,
        options,
    )
}

pub fn origin_result(result: &OriginResult, options: &ClusterOptions) -> Value {
    let total = result.origins.len();
    let items = result
        .origins
        .iter()
        .enumerate()
        .map(|(index, origin)| {
            ClusterInput::new(
                "origins",
                ResultScore::native(origin.confidence as f64, "origin_confidence"),
                serde_json::to_value(origin).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "symbol": result.symbol,
            "total_origins": result.total_origins,
            "truncated": result.truncated,
            "candidate_origins": total,
        }),
        items,
        options,
    )
}

pub fn entries_result(result: &EntriesResult, options: &ClusterOptions) -> Value {
    let total = result.entries.len();
    let items = result
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            ClusterInput::new(
                "entries",
                fallback_rank_score(index, total, "entry_rank"),
                serde_json::to_value(entry).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "shown": result.shown,
            "total": result.total,
        }),
        items,
        options,
    )
}

pub fn file_symbols_result(result: &FileSymbolsResult, options: &ClusterOptions) -> Value {
    let total = result.symbols.len();
    let items = result
        .symbols
        .iter()
        .enumerate()
        .map(|(index, symbol)| {
            ClusterInput::new(
                "symbols",
                fallback_rank_score(index, total, "source_order"),
                serde_json::to_value(symbol).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "file": result.file,
            "total": result.total,
        }),
        items,
        options,
    )
}

pub fn usages_result(result: &UsagesResult, options: &ClusterOptions) -> Value {
    let total_usages = result
        .records
        .iter()
        .map(|record| record.usages.len())
        .sum::<usize>();
    let mut original_index = 0usize;
    let mut record_summaries = Vec::new();
    let mut items = Vec::new();

    for (record_index, record) in result.records.iter().enumerate() {
        record_summaries.push(json!({
            "record": record.record,
            "total_usages": record.usages.len(),
        }));
        for (usage_index, usage) in record.usages.iter().enumerate() {
            let item = json!({
                "record": record.record,
                "usage": usage,
            });
            items.push(ClusterInput::new(
                format!("records[{record_index}].usages"),
                fallback_rank_score(usage_index, record.usages.len(), "l10n_usage_rank"),
                item,
                original_index,
            ));
            original_index += 1;
        }
    }

    clustered_value(
        json!({
            "query": result.query,
            "records": record_summaries,
            "total_usages": total_usages,
        }),
        items,
        options,
    )
}

pub fn changes_result(result: &ChangeReport, options: &ClusterOptions) -> Value {
    let total = result.affected_symbols.len();
    let items = result
        .affected_symbols
        .iter()
        .enumerate()
        .map(|(index, impact)| {
            let score = if impact.total_affected == 0 {
                0.35
            } else if impact.total_affected >= 20 {
                1.0
            } else {
                0.45 + ((impact.total_affected as f64 / 20.0) * 0.50)
            };
            ClusterInput::new(
                "affected_symbols",
                ResultScore::native(score, "impact_total"),
                serde_json::to_value(impact).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "changed_files": result.changed_files,
            "changed_symbols": result.changed_symbols,
            "total_affected_symbols": result.total_affected_symbols,
            "risk_summary": result.risk_summary,
            "candidate_affected_symbols": total,
        }),
        items,
        options,
    )
}

pub fn smells_result(result: &SmellsResult, options: &ClusterOptions) -> Value {
    let items = result
        .smells
        .iter()
        .enumerate()
        .map(|(index, smell)| {
            let ratio = if smell.threshold == 0 {
                1.0
            } else {
                smell.metric_value as f64 / smell.threshold as f64
            };
            let base = match smell.severity.as_str() {
                "critical" => 0.90,
                "warning" => 0.65,
                _ => 0.45,
            };
            let score = (base + (ratio.min(2.0) * 0.05)).clamp(0.0, 1.0);
            ClusterInput::new(
                "smells",
                ResultScore::fallback(score, "severity_metric"),
                serde_json::to_value(smell).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "total": result.total,
            "by_severity": result.by_severity,
        }),
        items,
        options,
    )
}

pub fn architecture_result(result: &ArchitectureResult, options: &ClusterOptions) -> Value {
    let items = result
        .violations
        .iter()
        .enumerate()
        .map(|(index, violation)| {
            ClusterInput::new(
                "violations",
                ResultScore::native(violation.confidence, "edge_confidence"),
                serde_json::to_value(violation).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "configured": result.configured,
            "total_violations": result.total_violations,
            "layers": result.layers,
        }),
        items,
        options,
    )
}

pub fn inferred_result(result: &InferredBuildResult, options: &ClusterOptions) -> Value {
    let candidate_records = result.records.len().min(options.candidate_limit);
    let items = result
        .records
        .iter()
        .take(options.candidate_limit)
        .enumerate()
        .map(|(index, record)| {
            ClusterInput::new(
                "records",
                ResultScore::native(record.confidence, "inferred_confidence"),
                serde_json::to_value(record).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "enabled": result.enabled,
            "saved": result.saved,
            "store_path": result.store_path,
            "total_records": result.total_records,
            "by_kind": result.by_kind,
            "candidate_records": candidate_records,
        }),
        items,
        options,
    )
}

fn concept_search_value<T, I>(
    query: &str,
    resolved_from: &str,
    matched_concept: &Option<String>,
    scopes: &[T],
    scores: I,
    options: &ClusterOptions,
) -> Value
where
    T: Serialize,
    I: IntoIterator<Item = f32>,
{
    let items = scopes
        .iter()
        .zip(scores)
        .enumerate()
        .map(|(index, (scope, score))| {
            ClusterInput::new(
                "scopes",
                concept_score(score),
                serde_json::to_value(scope).unwrap_or(Value::Null),
                index,
            )
        })
        .collect();

    clustered_value(
        json!({
            "query": query,
            "resolved_from": resolved_from,
            "matched_concept": matched_concept,
            "candidate_scopes": scopes.len(),
        }),
        items,
        options,
    )
}

fn concept_score(score: f32) -> ResultScore {
    ResultScore::native((score as f64) / 1000.0, "concept_score")
}

fn edge_score(graph: &Graph, source: &str, target: &str, kind: EdgeKind) -> Option<ResultScore> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.source == source && edge.target == target && edge.kind == kind)
        .map(|edge| edge.confidence)
        .max_by(f64::total_cmp)
        .map(|confidence| ResultScore::native(confidence, "edge_confidence"))
}

fn push_symbol_refs<F>(
    items: &mut Vec<ClusterInput<Value>>,
    section: &str,
    symbols: &[query::SymbolRef],
    score_fn: F,
) where
    F: Fn(&query::SymbolRef) -> Option<ResultScore>,
{
    let total = symbols.len();
    for (index, symbol) in symbols.iter().enumerate() {
        let score =
            score_fn(symbol).unwrap_or_else(|| fallback_rank_score(index, total, "section_rank"));
        items.push(ClusterInput::new(
            section,
            score,
            serde_json::to_value(symbol).unwrap_or(Value::Null),
            items.len(),
        ));
    }
}

fn push_scored_values<T: Serialize>(
    items: &mut Vec<ClusterInput<Value>>,
    section: &str,
    values: &[T],
    score: ResultScore,
) {
    for value in values {
        items.push(ClusterInput::new(
            section,
            score.clone(),
            serde_json::to_value(value).unwrap_or(Value::Null),
            items.len(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_expected_score_bands() {
        assert_eq!(
            ResultScore::native(0.95, "test").band(),
            ScoreBand::Excellent
        );
        assert_eq!(ResultScore::native(0.70, "test").band(), ScoreBand::Strong);
        assert_eq!(
            ResultScore::native(0.50, "test").band(),
            ScoreBand::Possible
        );
        assert_eq!(ResultScore::native(0.10, "test").band(), ScoreBand::Weak);
        assert_eq!(ResultScore::unknown("test").band(), ScoreBand::Unknown);
    }

    #[test]
    fn normalizes_bm25_scores_against_max_score() {
        let scores = normalize_bm25_scores(&[12.0, 6.0, 3.0]);
        assert_eq!(scores[0].normalized, Some(1.0));
        assert_eq!(scores[1].normalized, Some(0.5));
        assert_eq!(scores[2].normalized, Some(0.25));
        assert!(scores.iter().all(|score| score.native));
    }

    #[test]
    fn concept_scores_use_calibrated_scale() {
        assert_eq!(concept_score(1000.0).band(), ScoreBand::Excellent);
        assert_eq!(concept_score(850.0).band(), ScoreBand::Excellent);
        assert_eq!(concept_score(700.0).band(), ScoreBand::Strong);
        assert_eq!(concept_score(560.0).band(), ScoreBand::Possible);
    }

    #[test]
    fn fallback_order_stays_stable_inside_band() {
        let options = ClusterOptions::new(Some("possible".to_string()), 1, 10, 10);
        let result = clustered_result(
            json!({}),
            vec![
                ClusterInput::new("items", ResultScore::fallback(0.50, "rank"), json!("b"), 1),
                ClusterInput::new("items", ResultScore::fallback(0.55, "rank"), json!("a"), 0),
            ],
            &options,
        );

        let actual: Vec<&str> = result
            .page
            .items
            .iter()
            .map(|item| item.item.as_str().unwrap())
            .collect();
        assert_eq!(actual, vec!["a", "b"]);
    }

    #[test]
    fn native_scores_sort_descending_inside_band() {
        let options = ClusterOptions::new(Some("strong".to_string()), 1, 10, 10);
        let result = clustered_result(
            json!({}),
            vec![
                ClusterInput::new("items", ResultScore::native(0.70, "edge"), json!("b"), 0),
                ClusterInput::new("items", ResultScore::native(0.80, "edge"), json!("a"), 1),
            ],
            &options,
        );

        let actual: Vec<&str> = result
            .page
            .items
            .iter()
            .map(|item| item.item.as_str().unwrap())
            .collect();
        assert_eq!(actual, vec!["a", "b"]);
    }

    #[test]
    fn page_selects_requested_slice() {
        let options = ClusterOptions::new(Some("possible".to_string()), 2, 2, 10);
        let result = clustered_result(
            json!({}),
            (0..5)
                .map(|index| {
                    ClusterInput::new(
                        "items",
                        ResultScore::fallback(0.50, "rank"),
                        json!(index),
                        index,
                    )
                })
                .collect(),
            &options,
        );

        let actual: Vec<i64> = result
            .page
            .items
            .iter()
            .map(|item| item.item.as_i64().unwrap())
            .collect();
        assert_eq!(result.page.total_pages, 3);
        assert_eq!(actual, vec![2, 3]);
    }
}
