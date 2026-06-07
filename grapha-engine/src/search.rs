use std::path::Path;

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, STORED, STRING, Schema, TEXT, Value};
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

use crate::annotations::AnnotationIndex;
use crate::delta::{EntitySyncStats, GraphDelta, SyncMode};
use crate::fields::FieldSet;
use crate::snippet::compact_symbol_snippet;
use crate::symbol_locator::SymbolLocatorIndex;
use grapha_core::graph::{EdgeKind, Graph};
use grapha_core::graph::{Node, NodeRole};

const SEARCH_TERMS_COMPLETE_MATCH_BOOST: f32 = 2.0;
const SEARCH_TERMS_RELAXED_MATCH_BOOST: f32 = 1.5;
const EXACT_NAME_MATCH_BOOST: f32 = 20.0;

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub locator: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Default)]
pub struct SearchOptions {
    pub kind: Option<String>,
    pub module: Option<String>,
    pub repo: Option<String>,
    pub file_glob: Option<String>,
    pub role: Option<String>,
    pub fuzzy: bool,
    pub exact_name: bool,
    pub declarations_only: bool,
    pub public_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSyncStats {
    pub mode: SyncMode,
    pub documents: EntitySyncStats,
}

impl SearchSyncStats {
    pub fn from_graphs(previous: Option<&Graph>, graph: &Graph, mode: SyncMode) -> Self {
        let documents = match previous {
            Some(previous_graph) => GraphDelta::between(previous_graph, graph).node_stats(),
            None => EntitySyncStats::from_total(graph.nodes.len()),
        };
        Self { mode, documents }
    }

    pub fn summary(self) -> String {
        format!(
            "{} docs +{} ~{} -{}",
            self.mode.label(),
            self.documents.added,
            self.documents.updated,
            self.documents.deleted
        )
    }
}

#[derive(Clone, Copy)]
struct SearchFields {
    id: tantivy::schema::Field,
    locator: tantivy::schema::Field,
    name: tantivy::schema::Field,
    name_lower: tantivy::schema::Field,
    search_terms: Option<tantivy::schema::Field>,
    kind: tantivy::schema::Field,
    file: tantivy::schema::Field,
    module: tantivy::schema::Field,
    module_lower: tantivy::schema::Field,
    repo: Option<tantivy::schema::Field>,
    repo_lower: Option<tantivy::schema::Field>,
    visibility: tantivy::schema::Field,
    role: tantivy::schema::Field,
}

fn schema() -> (Schema, SearchFields) {
    let mut schema_builder = Schema::builder();
    let id = schema_builder.add_text_field("id", STRING | STORED);
    let locator = schema_builder.add_text_field("locator", TEXT | STORED);
    let name = schema_builder.add_text_field("name", TEXT | STORED);
    // Lowercased, non-tokenized name for fuzzy regex matching on CamelCase symbols
    let name_lower = schema_builder.add_text_field("name_lower", STRING);
    let search_terms = schema_builder.add_text_field("search_terms", TEXT);
    let kind = schema_builder.add_text_field("kind", STRING | STORED);
    let file = schema_builder.add_text_field("file", TEXT | STORED);
    let module = schema_builder.add_text_field("module", STRING | STORED);
    // Lowercased module for case-insensitive filtering
    let module_lower = schema_builder.add_text_field("module_lower", STRING);
    let repo = schema_builder.add_text_field("repo", STRING | STORED);
    let repo_lower = schema_builder.add_text_field("repo_lower", STRING);
    let visibility = schema_builder.add_text_field("visibility", STRING | STORED);
    let role = schema_builder.add_text_field("role", STRING | STORED);
    (
        schema_builder.build(),
        SearchFields {
            id,
            locator,
            name,
            name_lower,
            search_terms: Some(search_terms),
            kind,
            file,
            module,
            module_lower,
            repo: Some(repo),
            repo_lower: Some(repo_lower),
            visibility,
            role,
        },
    )
}

fn index_writer(index: &Index) -> Result<IndexWriter> {
    Ok(index.writer(50_000_000)?)
}

fn role_to_string(role: &Option<NodeRole>) -> String {
    match role {
        Some(NodeRole::EntryPoint) => "entry_point".to_string(),
        Some(NodeRole::Terminal { .. }) => "terminal".to_string(),
        Some(NodeRole::Internal) | None => "internal".to_string(),
    }
}

fn node_document(fields: SearchFields, node: &Node, locator: &str) -> Result<TantivyDocument> {
    let kind_str = serde_json::to_string(&node.kind)?
        .trim_matches('"')
        .to_string();
    let visibility_str = serde_json::to_string(&node.visibility)?
        .trim_matches('"')
        .to_string();
    let mut document = doc!(
        fields.id => node.id.clone(),
        fields.locator => locator.to_string(),
        fields.name => node.name.clone(),
        fields.name_lower => node.name.to_lowercase(),
        fields.kind => kind_str,
        fields.file => node.file.to_string_lossy().to_string(),
        fields.module => node.module.clone().unwrap_or_default(),
        fields.module_lower => node.module.as_deref().unwrap_or("").to_lowercase(),
        fields.visibility => visibility_str,
        fields.role => role_to_string(&node.role),
    );
    if let Some(repo_field) = fields.repo {
        document.add_text(repo_field, node.repo.as_deref().unwrap_or(""));
    }
    if let Some(repo_lower_field) = fields.repo_lower {
        document.add_text(
            repo_lower_field,
            node.repo.as_deref().unwrap_or("").to_lowercase(),
        );
    }
    if let Some(search_terms_field) = fields.search_terms {
        document.add_text(
            search_terms_field,
            search_terms_text(
                &node.name,
                locator,
                &node.file.to_string_lossy(),
                node.doc_comment.as_deref(),
                node.snippet.as_deref(),
            ),
        );
    }
    Ok(document)
}

fn rebuild_index_impl(graph: &Graph, index_path: &Path) -> Result<Index> {
    if index_path.exists() {
        std::fs::remove_dir_all(index_path)?;
    }
    std::fs::create_dir_all(index_path)?;
    let (schema, fields) = schema();
    let index = Index::create_in_dir(index_path, schema)?;
    let mut writer = index_writer(&index)?;
    let locators = SymbolLocatorIndex::new(graph);
    for node in &graph.nodes {
        writer.add_document(node_document(
            fields,
            node,
            &locators.locator_for_node(node),
        )?)?;
    }
    writer.commit()?;
    Ok(index)
}

pub fn build_index(graph: &Graph, index_path: &Path) -> Result<Index> {
    rebuild_index_impl(graph, index_path)
}

pub fn sync_index(
    previous: Option<&Graph>,
    graph: &Graph,
    index_path: &Path,
    force_full_rebuild: bool,
    precomputed_delta: Option<&GraphDelta>,
) -> Result<SearchSyncStats> {
    let full_stats = SearchSyncStats::from_graphs(previous, graph, SyncMode::FullRebuild);
    if force_full_rebuild || previous.is_none() || !index_path.exists() {
        rebuild_index_impl(graph, index_path)?;
        return Ok(full_stats);
    }

    let previous_graph = previous.expect("checked is_some above");
    let owned_delta;
    let delta = match precomputed_delta {
        Some(d) => d,
        None => {
            owned_delta = GraphDelta::between(previous_graph, graph);
            &owned_delta
        }
    };
    let incremental_stats = SearchSyncStats {
        mode: SyncMode::Incremental,
        documents: delta.node_stats(),
    };
    if requires_full_rebuild_for_locators(previous_graph, delta) {
        rebuild_index_impl(graph, index_path)?;
        return Ok(full_stats);
    }
    let index = match Index::open_in_dir(index_path) {
        Ok(index) => index,
        Err(_) => {
            rebuild_index_impl(graph, index_path)?;
            return Ok(full_stats);
        }
    };
    let fields = match resolve_fields(&index) {
        Ok(fields) => fields,
        Err(_) => {
            rebuild_index_impl(graph, index_path)?;
            return Ok(full_stats);
        }
    };
    if fields.repo.is_none() || fields.repo_lower.is_none() {
        rebuild_index_impl(graph, index_path)?;
        return Ok(full_stats);
    }

    let mut writer = index_writer(&index)?;
    let locators = SymbolLocatorIndex::new(graph);
    for node_id in &delta.deleted_node_ids {
        writer.delete_term(Term::from_field_text(fields.id, node_id));
    }
    for node in &delta.updated_nodes {
        writer.delete_term(Term::from_field_text(fields.id, &node.id));
    }
    for node in delta
        .added_nodes
        .iter()
        .copied()
        .chain(delta.updated_nodes.iter().copied())
    {
        writer.add_document(node_document(
            fields,
            node,
            &locators.locator_for_node(node),
        )?)?;
    }
    writer.commit()?;

    Ok(incremental_stats)
}

fn resolve_fields(index: &Index) -> Result<SearchFields> {
    let schema = index.schema();
    Ok(SearchFields {
        id: schema.get_field("id")?,
        locator: schema.get_field("locator")?,
        name: schema.get_field("name")?,
        name_lower: schema.get_field("name_lower")?,
        search_terms: schema.get_field("search_terms").ok(),
        kind: schema.get_field("kind")?,
        file: schema.get_field("file")?,
        module: schema.get_field("module")?,
        module_lower: schema.get_field("module_lower")?,
        repo: schema.get_field("repo").ok(),
        repo_lower: schema.get_field("repo_lower").ok(),
        visibility: schema.get_field("visibility")?,
        role: schema.get_field("role")?,
    })
}

#[allow(dead_code)] // Public backward-compat wrapper
pub fn search(index: &Index, query_str: &str, limit: usize) -> Result<Vec<SearchResult>> {
    search_filtered(index, query_str, limit, &SearchOptions::default())
}

/// Build a regex pattern for fuzzy matching on lowercased symbol names.
/// Inserts `.*` between characters to match substring with gaps, tolerating
/// typos, transpositions, and partial names.
/// "GiftPanle" → ".*g.*i.*f.*t.*p.*a.*n.*l.*e.*"
fn build_fuzzy_regex(query: &str) -> String {
    let lower = query.to_lowercase();
    let mut pattern = String::with_capacity(lower.len() * 4 + 4);
    pattern.push_str(".*");
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            // Escape regex metacharacters
            if "\\^$.|?*+()[]{}".contains(ch) {
                pattern.push('\\');
            }
            pattern.push(ch);
            pattern.push_str(".*");
        }
    }
    pattern
}

fn normalize_file_match_input(input: &str) -> String {
    input.replace('\\', "/").to_lowercase()
}

fn tokenize_locator_query(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_lowercase())
        .collect()
}

fn tokenize_search_terms(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;

    for ch in input.chars() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            previous = None;
            continue;
        }

        let starts_new_token = previous.is_some_and(|prev| {
            (prev.is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (prev.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (prev.is_ascii_digit() && ch.is_ascii_alphabetic())
        });
        if starts_new_token && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }

        current.push(ch.to_ascii_lowercase());
        previous = Some(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens.sort();
    tokens.dedup();
    tokens
}

fn search_terms_text(
    name: &str,
    locator: &str,
    file: &str,
    doc_comment: Option<&str>,
    snippet: Option<&str>,
) -> String {
    let mut tokens = tokenize_search_terms(name);
    tokens.extend(tokenize_search_terms(locator));
    tokens.extend(tokenize_search_terms(file));
    if let Some(doc_comment) = doc_comment {
        tokens.extend(tokenize_search_terms(doc_comment));
    }
    if let Some(snippet) = snippet {
        tokens.extend(tokenize_search_terms(snippet));
    }
    tokens.sort();
    tokens.dedup();
    tokens.join(" ")
}

fn identifier_like_query(query: &str) -> bool {
    !query.is_empty()
        && query
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
}

fn search_terms_complete_query(fields: SearchFields, query_str: &str) -> Option<Box<dyn Query>> {
    let search_terms_field = fields.search_terms?;
    let terms = tokenize_search_terms(query_str);
    if terms.is_empty() {
        return None;
    }

    let clauses: Vec<(Occur, Box<dyn Query>)> = terms
        .into_iter()
        .map(|term| {
            let term = Term::from_field_text(search_terms_field, &term);
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
            )
        })
        .collect();
    Some(Box::new(BooleanQuery::new(clauses)))
}

fn search_terms_relaxed_query(fields: SearchFields, query_str: &str) -> Option<Box<dyn Query>> {
    let search_terms_field = fields.search_terms?;
    let terms = tokenize_search_terms(query_str);
    if terms.len() < 2 {
        return None;
    }

    let minimum_required = search_terms_minimum_should_match(terms.len());
    let clauses: Vec<(Occur, Box<dyn Query>)> = terms
        .into_iter()
        .map(|term| {
            let term = Term::from_field_text(search_terms_field, &term);
            (
                Occur::Should,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
            )
        })
        .collect();
    Some(Box::new(BooleanQuery::with_minimum_required_clauses(
        clauses,
        minimum_required,
    )))
}

fn search_terms_minimum_should_match(term_count: usize) -> usize {
    match term_count {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => (term_count * 2).div_ceil(3),
    }
}

fn exact_name_query(fields: SearchFields, query_str: &str) -> Option<Box<dyn Query>> {
    let query_name = normalized_exact_name(query_str);
    if query_name.is_empty() {
        return None;
    }

    let term = Term::from_field_text(fields.name_lower, &query_name);
    Some(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
}

fn requires_full_rebuild_for_locators(previous: &Graph, delta: &GraphDelta<'_>) -> bool {
    if delta
        .added_edges
        .iter()
        .chain(delta.updated_edges.iter())
        .any(|edge| edge.edge.kind == EdgeKind::Contains)
    {
        return true;
    }

    if delta.deleted_edge_ids.is_empty() {
        return false;
    }

    previous.edges.iter().any(|edge| {
        edge.kind == EdgeKind::Contains
            && delta
                .deleted_edge_ids
                .iter()
                .any(|deleted| deleted == &crate::delta::edge_fingerprint(edge))
    })
}

fn normalized_exact_name(name: &str) -> String {
    let trimmed = name.trim();
    trimmed
        .split('(')
        .next()
        .unwrap_or(trimmed)
        .trim()
        .to_lowercase()
}

fn is_accessor_symbol(id: &str) -> bool {
    id.contains("functiongetter:")
        || id.contains("functionsetter:")
        || id.contains("getter:")
        || id.contains("setter:")
}

fn is_declaration_result(result: &SearchResult) -> bool {
    !matches!(result.kind.as_str(), "view" | "branch") && !is_accessor_symbol(&result.id)
}

fn exact_match_rank(result: &SearchResult, query_name: &str) -> usize {
    usize::from(normalized_exact_name(&result.name) != query_name)
}

fn declaration_rank(result: &SearchResult) -> usize {
    if !is_declaration_result(result) {
        return 5;
    }

    match result.kind.as_str() {
        "class" | "struct" | "enum" | "trait" | "protocol" | "module" | "type_alias"
        | "extension" => 0,
        "function" => 1,
        "property" | "constant" | "field" | "variant" => 2,
        "impl" => 3,
        _ => 4,
    }
}

fn has_glob_metacharacters(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

fn build_file_filter_regex(pattern: &str) -> Result<Regex> {
    let normalized = normalize_file_match_input(pattern);
    let mut regex = String::new();

    if has_glob_metacharacters(&normalized) {
        regex.push('^');
        for ch in normalized.chars() {
            match ch {
                '*' => regex.push_str(".*"),
                '?' => regex.push('.'),
                _ => regex.push_str(&regex::escape(&ch.to_string())),
            }
        }
        regex.push('$');
    } else {
        regex.push_str("^.*");
        regex.push_str(&regex::escape(&normalized));
        regex.push('$');
    }

    Ok(Regex::new(&regex)?)
}

pub fn search_filtered(
    index: &Index,
    query_str: &str,
    limit: usize,
    options: &SearchOptions,
) -> Result<Vec<SearchResult>> {
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();

    let fields = resolve_fields(index)?;

    let text_query: Box<dyn tantivy::query::Query> = if options.fuzzy {
        // Fuzzy search: use regex on the lowercased, non-tokenized name field.
        // This handles CamelCase symbols correctly — "GiftPanle" matches
        // "giftpanelviewmodel" via substring, unlike FuzzyTermQuery which
        // requires the entire token to be within edit distance.
        let pattern = build_fuzzy_regex(query_str);
        Box::new(tantivy::query::RegexQuery::from_pattern(
            &pattern,
            fields.name_lower,
        )?)
    } else if query_str.contains("::") {
        let terms = tokenize_locator_query(query_str);
        let clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = terms
            .into_iter()
            .map(|term| {
                let term = Term::from_field_text(fields.locator, &term);
                (
                    Occur::Must,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                        as Box<dyn tantivy::query::Query>,
                )
            })
            .collect();
        Box::new(BooleanQuery::new(clauses))
    } else {
        let query_parser =
            QueryParser::for_index(index, vec![fields.name, fields.locator, fields.file]);
        let exact_query = Box::new(query_parser.parse_query(query_str)?) as Box<dyn Query>;
        let mut query_clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Should, exact_query)];
        let identifier_like = identifier_like_query(query_str);
        if identifier_like && let Some(name_query) = exact_name_query(fields, query_str) {
            query_clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(name_query, EXACT_NAME_MATCH_BOOST)),
            ));
        }
        if let Some(token_query) = search_terms_complete_query(fields, query_str) {
            if identifier_like {
                query_clauses.push((Occur::Should, token_query));
            } else {
                query_clauses.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        token_query,
                        SEARCH_TERMS_COMPLETE_MATCH_BOOST,
                    )),
                ));
            }
        }
        if !identifier_like && let Some(token_query) = search_terms_relaxed_query(fields, query_str)
        {
            query_clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    token_query,
                    SEARCH_TERMS_RELAXED_MATCH_BOOST,
                )),
            ));
        }
        Box::new(BooleanQuery::new(query_clauses))
    };

    let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![(Occur::Must, text_query)];

    if let Some(ref kind_filter) = options.kind {
        let term = Term::from_field_text(fields.kind, kind_filter);
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    } else {
        let term = Term::from_field_text(fields.kind, "extension");
        clauses.push((
            Occur::MustNot,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    if let Some(ref module_filter) = options.module {
        // Case-insensitive module matching: store a lowercased module field
        // and always query lowercase. Since module is STRING (exact match),
        // we use the module_lower field for case-insensitive matching.
        let term = Term::from_field_text(fields.module_lower, &module_filter.to_lowercase());
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    if let Some(ref repo_filter) = options.repo {
        let Some(repo_lower_field) = fields.repo_lower else {
            return Ok(Vec::new());
        };
        let term = Term::from_field_text(repo_lower_field, &repo_filter.to_lowercase());
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    if let Some(ref role_filter) = options.role {
        let term = Term::from_field_text(fields.role, role_filter);
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    if options.public_only {
        let term = Term::from_field_text(fields.visibility, "public");
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }

    let final_query = BooleanQuery::new(clauses);
    let file_filter = options
        .file_glob
        .as_deref()
        .map(build_file_filter_regex)
        .transpose()?;
    let requires_full_candidate_scan =
        file_filter.is_some() || options.exact_name || options.declarations_only;
    let candidate_limit = if requires_full_candidate_scan {
        searcher.search(&final_query, &Count)?
    } else {
        limit
    };

    if candidate_limit == 0 {
        return Ok(Vec::new());
    }

    let top_docs = searcher.search(
        &final_query,
        &TopDocs::with_limit(candidate_limit).order_by_score(),
    )?;

    let mut results = Vec::new();
    let normalized_query_name = options.exact_name.then(|| normalized_exact_name(query_str));
    for (score, doc_address) in top_docs {
        let doc: TantivyDocument = searcher.doc(doc_address)?;
        let get_str = |field| {
            doc.get_first(field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let file_val = get_str(fields.file);
        if let Some(regex) = &file_filter
            && !regex.is_match(&normalize_file_match_input(&file_val))
        {
            continue;
        }
        let module_val = get_str(fields.module);
        let repo_val = fields.repo.map(&get_str).unwrap_or_default();
        let role_val = get_str(fields.role);
        let result = SearchResult {
            id: get_str(fields.id),
            locator: get_str(fields.locator),
            name: get_str(fields.name),
            kind: get_str(fields.kind),
            file: file_val,
            score,
            module: if module_val.is_empty() {
                None
            } else {
                Some(module_val)
            },
            repo: if repo_val.is_empty() {
                None
            } else {
                Some(repo_val)
            },
            role: if role_val.is_empty() {
                None
            } else {
                Some(role_val)
            },
        };
        if let Some(query_name) = normalized_query_name.as_deref()
            && normalized_exact_name(&result.name) != query_name
        {
            continue;
        }
        if options.declarations_only && !is_declaration_result(&result) {
            continue;
        }
        results.push(result);
        if results.len() == limit {
            break;
        }
    }

    if query_str.contains("::") {
        let query_lower = query_str.to_lowercase();
        results.sort_by(|left, right| {
            locator_rank(&left.locator, &query_lower)
                .cmp(&locator_rank(&right.locator, &query_lower))
                .then_with(|| search_kind_rank(&left.kind).cmp(&search_kind_rank(&right.kind)))
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left.locator.cmp(&right.locator))
        });
    } else if identifier_like_query(query_str) && !options.fuzzy {
        let query_name = normalized_exact_name(query_str);
        results.sort_by(|left, right| {
            exact_match_rank(left, &query_name)
                .cmp(&exact_match_rank(right, &query_name))
                .then_with(|| declaration_rank(left).cmp(&declaration_rank(right)))
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left.locator.cmp(&right.locator))
        });
    }

    Ok(results)
}

fn search_kind_rank(kind: &str) -> usize {
    match kind {
        "function" => 0,
        "property" => 1,
        "variant" | "field" => 2,
        "class" | "struct" | "enum" | "trait" | "module" | "constant" | "type_alias"
        | "protocol" => 3,
        "impl" | "extension" => 4,
        "view" | "branch" => 5,
        _ => 6,
    }
}

fn locator_rank(locator: &str, query_lower: &str) -> usize {
    let locator_lower = locator.to_lowercase();
    if locator_lower == query_lower {
        0
    } else if locator_lower.ends_with(&format!("::{query_lower}")) {
        1
    } else if locator_lower.contains(query_lower) {
        2
    } else {
        3
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchOutputResult {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub called_by: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub type_refs: Vec<String>,
}

struct GraphSearchDetails<'a> {
    node: &'a Node,
    calls: Vec<String>,
    called_by: Vec<String>,
    type_refs: Vec<String>,
}

fn visibility_to_string(node: &Node) -> String {
    serde_json::to_string(&node.visibility)
        .unwrap_or_else(|_| format!("{:?}", node.visibility))
        .trim_matches('"')
        .to_string()
}

fn role_value(role: &Option<NodeRole>) -> Option<String> {
    role.as_ref().map(|role| match role {
        NodeRole::EntryPoint => "entry_point".to_string(),
        NodeRole::Terminal { .. } => "terminal".to_string(),
        NodeRole::Internal => "internal".to_string(),
    })
}

fn node_span_string(node: &Node) -> String {
    format!(
        "{}:{}-{}:{}",
        node.span.start[0], node.span.start[1], node.span.end[0], node.span.end[1]
    )
}

fn collect_graph_details<'a>(
    results: &[SearchResult],
    graph: &'a Graph,
) -> Vec<Option<GraphSearchDetails<'a>>> {
    results
        .iter()
        .map(|result| {
            let node = graph.nodes.iter().find(|node| node.id == result.id)?;
            let calls = graph
                .edges
                .iter()
                .filter(|e| e.source == result.id && e.kind == EdgeKind::Calls)
                .map(|e| e.target.clone())
                .collect();
            let called_by = graph
                .edges
                .iter()
                .filter(|e| e.target == result.id && e.kind == EdgeKind::Calls)
                .map(|e| e.source.clone())
                .collect();
            let type_refs = graph
                .edges
                .iter()
                .filter(|e| e.source == result.id && e.kind == EdgeKind::TypeRef)
                .map(|e| e.target.clone())
                .collect();
            Some(GraphSearchDetails {
                node,
                calls,
                called_by,
                type_refs,
            })
        })
        .collect()
}

pub fn needs_graph_for_projection(fields: FieldSet, include_context: bool) -> bool {
    include_context
        || fields.span
        || fields.snippet
        || fields.visibility
        || fields.signature
        || fields.doc_comment
        || fields.annotation
        || fields.role
}

pub fn project_results(
    results: &[SearchResult],
    graph: Option<&Graph>,
    fields: FieldSet,
    include_context: bool,
    annotations: Option<&AnnotationIndex>,
) -> Vec<SearchOutputResult> {
    let graph_details = graph.map(|graph| collect_graph_details(results, graph));

    results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let details = graph_details
                .as_ref()
                .and_then(|details| details.get(index))
                .and_then(|details| details.as_ref());
            let role = if fields.role {
                details
                    .map(|details| role_value(&details.node.role))
                    .unwrap_or_else(|| result.role.clone())
            } else {
                None
            };
            SearchOutputResult {
                name: result.name.clone(),
                kind: result.kind.clone(),
                score: fields.score.then_some(result.score),
                file: fields.file.then(|| result.file.clone()),
                id: fields.id.then(|| result.id.clone()),
                locator: fields.locator.then(|| result.locator.clone()),
                module: if fields.module {
                    result.module.clone()
                } else {
                    None
                },
                repo: if fields.repo {
                    result.repo.clone()
                } else {
                    None
                },
                span: if fields.span {
                    details.map(|details| node_span_string(details.node))
                } else {
                    None
                },
                snippet: if fields.snippet {
                    details
                        .and_then(|details| details.node.snippet.as_deref())
                        .map(compact_symbol_snippet)
                } else {
                    None
                },
                visibility: if fields.visibility {
                    details.map(|details| visibility_to_string(details.node))
                } else {
                    None
                },
                signature: if fields.signature {
                    details.and_then(|details| details.node.signature.clone())
                } else {
                    None
                },
                doc_comment: if fields.doc_comment {
                    details.and_then(|details| details.node.doc_comment.clone())
                } else {
                    None
                },
                annotation: if fields.annotation {
                    details.and_then(|details| {
                        annotations
                            .and_then(|annotations| annotations.get_for_node(details.node))
                            .map(|annotation| annotation.text)
                    })
                } else {
                    None
                },
                role,
                calls: if include_context {
                    details
                        .map(|details| details.calls.clone())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                },
                called_by: if include_context {
                    details
                        .map(|details| details.called_by.clone())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                },
                type_refs: if include_context {
                    details
                        .map(|details| details.type_refs.clone())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap;
    use tantivy::collector::Count;
    use tantivy::query::AllQuery;

    fn make_test_graph() -> Graph {
        let mk = |id: &str, name: &str, kind: NodeKind, file: &str| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: file.into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };
        Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("a.rs::Config", "Config", NodeKind::Struct, "a.rs"),
                mk(
                    "a.rs::default_config",
                    "default_config",
                    NodeKind::Function,
                    "a.rs",
                ),
                mk("b.rs::run", "run", NodeKind::Function, "b.rs"),
            ],
            edges: vec![],
        }
    }

    fn make_rich_test_graph() -> Graph {
        let mk = |id: &str,
                  name: &str,
                  kind: NodeKind,
                  file: &str,
                  module: Option<&str>,
                  role: Option<NodeRole>| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: file.into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role,
            signature: None,
            doc_comment: None,
            module: module.map(String::from),
            snippet: None,
            repo: None,
        };
        Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk(
                    "app::AppView",
                    "AppView",
                    NodeKind::Struct,
                    "Sources/App/AppView.swift",
                    Some("App"),
                    Some(NodeRole::EntryPoint),
                ),
                mk(
                    "app::fetch_data",
                    "fetch_data",
                    NodeKind::Function,
                    "Sources/App/Network.swift",
                    Some("App"),
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                mk(
                    "core::Config",
                    "Config",
                    NodeKind::Struct,
                    "Sources/Core/Config.swift",
                    Some("Core"),
                    None,
                ),
                mk(
                    "core::save_config",
                    "save_config",
                    NodeKind::Function,
                    "Sources/Core/Persist.swift",
                    Some("Core"),
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![],
        }
    }

    fn make_locator_tiebreak_graph() -> Graph {
        let mk = |id: &str, name: &str, kind: NodeKind, file: &str| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: file.into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("ModuleExport".into()),
            snippet: None,
            repo: None,
        };

        Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("Hello.swift::Test", "Test", NodeKind::Class, "Hello.swift"),
                mk(
                    "Hello.swift::ext_Test",
                    "Test",
                    NodeKind::Extension,
                    "Hello.swift",
                ),
            ],
            edges: vec![],
        }
    }

    #[test]
    fn search_finds_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let results = search(&index, "Config", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.name == "Config"));
    }

    #[test]
    fn search_returns_empty_for_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let results = search(&index, "zzzznonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn sync_index_updates_added_updated_and_deleted_documents() {
        let dir = tempfile::tempdir().unwrap();
        let previous = make_test_graph();
        build_index(&previous, dir.path()).unwrap();

        let mut updated_node = previous.nodes[0].clone();
        updated_node.name = "RuntimeConfig".to_string();
        let next = Graph {
            version: previous.version.clone(),
            nodes: vec![
                updated_node,
                previous.nodes[2].clone(),
                Node {
                    id: "c.rs::fresh".to_string(),
                    kind: NodeKind::Function,
                    name: "fresh".to_string(),
                    file: "c.rs".into(),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };

        let stats = sync_index(Some(&previous), &next, dir.path(), false, None).unwrap();
        assert_eq!(stats.mode, SyncMode::Incremental);
        assert_eq!(
            stats.documents,
            EntitySyncStats {
                added: 1,
                updated: 1,
                deleted: 1,
            }
        );

        let index = Index::open_in_dir(dir.path()).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        assert_eq!(searcher.search(&AllQuery, &Count).unwrap(), 3);

        let results = search(&index, "RuntimeConfig", 10).unwrap();
        assert_eq!(results.len(), 1);
        let deleted = search(&index, "default_config", 10).unwrap();
        assert!(deleted.is_empty());
    }

    #[test]
    fn search_without_filters_backward_compat() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let results = search(&index, "Config", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.name == "Config"));
    }

    #[test]
    fn search_hides_extension_symbols_unless_kind_filter_requests_them() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_locator_tiebreak_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let results =
            search_filtered(&index, "Hello.swift::Test", 10, &SearchOptions::default()).unwrap();

        assert!(results.iter().any(|result| result.kind == "class"));
        assert!(results.iter().all(|result| result.kind != "extension"));

        let extension_results = search_filtered(
            &index,
            "Hello.swift::Test",
            10,
            &SearchOptions {
                kind: Some("extension".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(extension_results.len(), 1);
        assert_eq!(extension_results[0].kind, "extension");
    }

    #[test]
    fn identifier_search_matches_token_equivalent_wrapper_name() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "AppUI/Sources/AppResource/Generated/Strings.generated.swift::L10n::commonuiListSearchEmpty".into(),
                    kind: NodeKind::Property,
                    name: "commonuiListSearchEmpty".into(),
                    file: "AppUI/Sources/AppResource/Generated/Strings.generated.swift".into(),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("AppUI".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "AppUI/Sources/AppResource/Generated/Strings.generated.swift::L10n::roomShareNoFriedns".into(),
                    kind: NodeKind::Property,
                    name: "roomShareNoFriedns".into(),
                    file: "AppUI/Sources/AppResource/Generated/Strings.generated.swift".into(),
                    span: Span {
                        start: [2, 0],
                        end: [3, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("AppUI".into()),
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search(&index, "commonuiSearchListEmpty", 10).unwrap();

        assert!(
            results
                .iter()
                .any(|result| result.name == "commonuiListSearchEmpty"),
            "tokenized identifier search should find the real generated wrapper, got: {:?}",
            results
                .iter()
                .map(|result| &result.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn exact_name_matches_function_base_name_without_signature_noise() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "s:12ModuleExport16routeProfileHome3uidyx_tSzRzlF".into(),
                    kind: NodeKind::Function,
                    name: "routeProfileHome(uid:)".into(),
                    file: "ProfileUtil.swift".into(),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("ModuleExport".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "other".into(),
                    kind: NodeKind::Function,
                    name: "routeProfileHomeElsewhere(uid:)".into(),
                    file: "Other.swift".into(),
                    span: Span {
                        start: [2, 0],
                        end: [3, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("Other".into()),
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search_filtered(
            &index,
            "routeProfileHome",
            10,
            &SearchOptions {
                exact_name: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "routeProfileHome(uid:)");
    }

    #[test]
    fn declarations_only_excludes_accessors_and_synthetic_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "type".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: "Body.swift".into(),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "functiongetter:body".into(),
                    kind: NodeKind::Function,
                    name: "body".into(),
                    file: "Body.swift".into(),
                    span: Span {
                        start: [2, 0],
                        end: [3, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view".into(),
                    kind: NodeKind::View,
                    name: "body".into(),
                    file: "Body.swift".into(),
                    span: Span {
                        start: [4, 0],
                        end: [5, 0],
                    },
                    visibility: Visibility::Private,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search_filtered(
            &index,
            "body",
            10,
            &SearchOptions {
                declarations_only: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "property");
    }

    #[test]
    fn public_only_filters_private_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "public".into(),
                    kind: NodeKind::Function,
                    name: "run".into(),
                    file: "run.rs".into(),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "private".into(),
                    kind: NodeKind::Function,
                    name: "run".into(),
                    file: "run.rs".into(),
                    span: Span {
                        start: [2, 0],
                        end: [3, 0],
                    },
                    visibility: Visibility::Private,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search_filtered(
            &index,
            "run",
            10,
            &SearchOptions {
                public_only: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "public");
    }

    #[test]
    fn identifier_search_prefers_real_declaration_over_synthetic_match() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "synthetic".into(),
                    kind: NodeKind::View,
                    name: "UserNameView".into(),
                    file: "Synthetic.swift".into(),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Private,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "real".into(),
                    kind: NodeKind::Struct,
                    name: "UserNameView".into(),
                    file: "UserNameView.swift".into(),
                    span: Span {
                        start: [2, 0],
                        end: [3, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("ModuleExport".into()),
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search(&index, "UserNameView", 10).unwrap();

        assert_eq!(
            results.first().map(|result| result.kind.as_str()),
            Some("struct")
        );
    }

    #[test]
    fn filter_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            kind: Some("struct".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "Config", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Config");
        assert_eq!(results[0].kind, "struct");
    }

    #[test]
    fn filter_by_module() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            module: Some("Core".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "config", 10, &options).unwrap();
        assert!(!results.is_empty());
        for r in &results {
            assert_eq!(r.module.as_deref(), Some("Core"));
        }
    }

    #[test]
    fn filter_by_repo() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = make_rich_test_graph();
        graph.nodes[0].repo = Some("app".into());
        graph.nodes[1].repo = Some("app".into());
        graph.nodes[2].repo = Some("shared".into());
        graph.nodes[3].repo = Some("shared".into());
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            repo: Some("shared".into()),
            ..Default::default()
        };

        let results = search_filtered(&index, "config", 10, &options).unwrap();

        assert!(!results.is_empty());
        for result in &results {
            assert_eq!(result.repo.as_deref(), Some("shared"));
        }
    }

    #[test]
    fn search_matches_doc_comment_terms() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = make_rich_test_graph();
        graph.nodes[2].doc_comment = Some("Coordinates the gift flow handoff.".into());
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search_filtered(&index, "gift flow", 10, &SearchOptions::default()).unwrap();

        assert!(
            results.iter().any(|result| result.name == "Config"),
            "doc-only business terms should find the documented symbol, got: {:?}",
            results
                .iter()
                .map(|result| &result.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_matches_snippet_terms() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = make_rich_test_graph();
        graph.nodes[2].snippet =
            Some("fn render() { filter_search_results_by_current_scope(); }".into());
        let index = build_index(&graph, dir.path()).unwrap();

        let results = search_filtered(
            &index,
            "filter search results scope",
            10,
            &SearchOptions::default(),
        )
        .unwrap();

        assert!(
            results.iter().any(|result| result.name == "Config"),
            "body/snippet terms should find the symbol, got: {:?}",
            results
                .iter()
                .map(|result| &result.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn natural_language_search_prefers_high_coverage_doc_comment_over_token_noise() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |id: String, name: String, file: String, doc_comment: Option<String>| Node {
            id,
            kind: NodeKind::Function,
            name,
            file: file.into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment,
            module: Some("Search".into()),
            snippet: None,
            repo: None,
        };
        let mut nodes = Vec::new();
        for index in 0..30 {
            nodes.push(mk(
                format!("noise::search_results_{index}"),
                format!("search_results_{index}"),
                format!("src/search_results_{index}.rs"),
                None,
            ));
            nodes.push(mk(
                format!("noise::scope_filter_{index}"),
                format!("scope_filter_{index}"),
                format!("src/scope_filter_{index}.rs"),
                None,
            ));
        }
        nodes.push(mk(
            "target::chunk_in_scope".into(),
            "chunk_in_scope".into(),
            "src/chunks.rs".into(),
            Some("Filters search results to the active scope before ranking chunks.".into()),
        ));
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes,
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let natural_results = search_filtered(
            &index,
            "filters search results by scope",
            10,
            &SearchOptions::default(),
        )
        .unwrap();
        assert!(
            natural_results
                .iter()
                .any(|result| result.name == "chunk_in_scope"),
            "doc-comment behavior phrase should rank within the default limit, got: {:?}",
            natural_results
                .iter()
                .map(|result| (&result.name, result.score))
                .collect::<Vec<_>>()
        );

        let identifier_results =
            search_filtered(&index, "chunk_in_scope", 10, &SearchOptions::default()).unwrap();
        assert_eq!(
            identifier_results
                .first()
                .map(|result| result.name.as_str()),
            Some("chunk_in_scope"),
            "exact identifier search should still prefer the target, got: {:?}",
            identifier_results
                .iter()
                .map(|result| (&result.name, result.score))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn identifier_search_keeps_exact_name_ahead_of_conceptual_docstore_family() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |id: String,
                  name: String,
                  kind: NodeKind,
                  file: String,
                  doc_comment: Option<String>| Node {
            id,
            kind,
            name,
            file: file.into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment,
            module: Some("Storage".into()),
            snippet: None,
            repo: None,
        };
        let mut nodes = vec![mk(
            "storage::DocStore".into(),
            "DocStore".into(),
            NodeKind::Trait,
            "src/storage/store.rs".into(),
            None,
        )];
        for index in 0..30 {
            nodes.push(mk(
                format!("storage::sqlite_docstore_{index}"),
                format!("impl DocStore for SqliteDocStore{index}"),
                NodeKind::Impl,
                "src/storage/docstore.rs".into(),
                Some("Persists document chunks into SQLite storage.".into()),
            ));
        }
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes,
            edges: vec![],
        };
        let index = build_index(&graph, dir.path()).unwrap();

        let identifier_results =
            search_filtered(&index, "DocStore", 10, &SearchOptions::default()).unwrap();
        let exact_rank = identifier_results
            .iter()
            .position(|result| result.name == "DocStore");
        assert!(
            exact_rank.is_some_and(|rank| rank < 3),
            "exact DocStore symbol should rank in the top 3, got: {:?}",
            identifier_results
                .iter()
                .map(|result| (&result.name, result.score))
                .collect::<Vec<_>>()
        );

        let lowercase_identifier_results =
            search_filtered(&index, "docstore", 10, &SearchOptions::default()).unwrap();
        let lowercase_exact_rank = lowercase_identifier_results
            .iter()
            .position(|result| result.name == "DocStore");
        assert!(
            lowercase_exact_rank.is_some_and(|rank| rank < 3),
            "case-insensitive exact DocStore match should rank in the top 3, got: {:?}",
            lowercase_identifier_results
                .iter()
                .map(|result| (&result.name, result.score))
                .collect::<Vec<_>>()
        );

        let conceptual_results = search_filtered(
            &index,
            "persists document chunks into sqlite",
            10,
            &SearchOptions::default(),
        )
        .unwrap();
        assert!(
            conceptual_results
                .iter()
                .any(|result| result.name.contains("SqliteDocStore")),
            "conceptual phrase should still surface the behaviorally relevant store, got: {:?}",
            conceptual_results
                .iter()
                .map(|result| (&result.name, result.score))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sync_index_rebuilds_legacy_index_without_repo_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut schema_builder = Schema::builder();
        let id = schema_builder.add_text_field("id", STRING | STORED);
        let locator = schema_builder.add_text_field("locator", TEXT | STORED);
        let name = schema_builder.add_text_field("name", TEXT | STORED);
        let name_lower = schema_builder.add_text_field("name_lower", STRING);
        schema_builder.add_text_field("search_terms", TEXT);
        let kind = schema_builder.add_text_field("kind", STRING | STORED);
        let file = schema_builder.add_text_field("file", TEXT | STORED);
        let module = schema_builder.add_text_field("module", STRING | STORED);
        let module_lower = schema_builder.add_text_field("module_lower", STRING);
        let visibility = schema_builder.add_text_field("visibility", STRING | STORED);
        let role = schema_builder.add_text_field("role", STRING | STORED);
        let index = Index::create_in_dir(dir.path(), schema_builder.build()).unwrap();
        let mut writer = index.writer(50_000_000).unwrap();
        writer
            .add_document(tantivy::doc!(
                id => "legacy",
                locator => "legacy",
                name => "Config",
                name_lower => "config",
                kind => "struct",
                file => "Config.swift",
                module => "Core",
                module_lower => "core",
                visibility => "public",
                role => "internal",
            ))
            .unwrap();
        writer.commit().unwrap();

        let mut graph = make_rich_test_graph();
        for node in &mut graph.nodes {
            node.repo = Some("shared".into());
        }

        let stats = sync_index(Some(&graph), &graph, dir.path(), false, None).unwrap();
        assert_eq!(stats.mode, SyncMode::FullRebuild);

        let rebuilt = Index::open_in_dir(dir.path()).unwrap();
        let options = SearchOptions {
            repo: Some("shared".into()),
            ..Default::default()
        };
        let results = search_filtered(&rebuilt, "Config", 10, &options).unwrap();
        assert!(!results.is_empty());
        assert!(
            results
                .iter()
                .all(|result| result.repo.as_deref() == Some("shared"))
        );
    }

    #[test]
    fn filter_by_role_entry_point() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            role: Some("entry_point".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "AppView", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "AppView");
        assert_eq!(results[0].role.as_deref(), Some("entry_point"));
    }

    #[test]
    fn filter_by_role_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            role: Some("terminal".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "fetch_data", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].role.as_deref(), Some("terminal"));
    }

    #[test]
    fn filter_by_role_internal() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            role: Some("internal".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "Config", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Config");
        assert_eq!(results[0].role.as_deref(), Some("internal"));
    }

    #[test]
    fn filter_by_file_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            file_glob: Some("Config.swift".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "Config", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Config");
        assert_eq!(results[0].file, "Sources/Core/Config.swift");
    }

    #[test]
    fn filter_by_file_glob() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            file_glob: Some("Sources/*/Persist.swift".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "save_config", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "save_config");
        assert_eq!(results[0].file, "Sources/Core/Persist.swift");
    }

    #[test]
    fn fuzzy_search_finds_misspelled() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            fuzzy: true,
            ..Default::default()
        };
        let results = search_filtered(&index, "confg", 10, &options).unwrap();
        assert!(
            results.iter().any(|r| r.name == "Config"),
            "fuzzy search should find 'Config' for misspelling 'confg', got: {:?}",
            results.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn combined_kind_and_module_filter() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        let options = SearchOptions {
            kind: Some("function".into()),
            module: Some("App".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "fetch_data", 10, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "fetch_data");
        assert_eq!(results[0].module.as_deref(), Some("App"));
    }

    #[test]
    fn filter_excludes_non_matching() {
        let dir = tempfile::tempdir().unwrap();
        let graph = make_rich_test_graph();
        let index = build_index(&graph, dir.path()).unwrap();
        // AppView is a struct; filtering by kind=function should exclude it
        let options = SearchOptions {
            kind: Some("function".into()),
            ..Default::default()
        };
        let results = search_filtered(&index, "AppView", 10, &options).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn projection_respects_fields_and_context() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "app::main".into(),
                    kind: NodeKind::Function,
                    name: "main".into(),
                    file: "src/main.rs".into(),
                    span: Span {
                        start: [1, 0],
                        end: [3, 1],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: Some(NodeRole::EntryPoint),
                    signature: Some("fn main()".into()),
                    doc_comment: Some("Starts the application".into()),
                    module: Some("App".into()),
                    snippet: Some("\n    fn main() {\n        helper();\n    }\n\n\n".into()),
                    repo: None,
                },
                Node {
                    id: "app::helper".into(),
                    kind: NodeKind::Function,
                    name: "helper".into(),
                    file: "src/main.rs".into(),
                    span: Span {
                        start: [5, 0],
                        end: [5, 12],
                    },
                    visibility: Visibility::Private,
                    metadata: HashMap::new(),
                    role: None,
                    signature: Some("fn helper()".into()),
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: Some("fn helper() {}".into()),
                    repo: None,
                },
            ],
            edges: vec![Edge {
                source: "app::main".into(),
                target: "app::helper".into(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: Some(false),
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let results = vec![SearchResult {
            id: "app::main".into(),
            locator: "App::main.rs::main".into(),
            name: "main".into(),
            kind: "function".into(),
            file: "src/main.rs".into(),
            score: 1.0,
            module: Some("App".into()),
            repo: Some("app".into()),
            role: Some("entry_point".into()),
        }];

        let projected = project_results(
            &results,
            Some(&graph),
            FieldSet::parse("id,repo,signature,doc_comment,role,snippet"),
            true,
            None,
        );

        assert_eq!(projected.len(), 1);
        let result = &projected[0];
        assert_eq!(result.name, "main");
        assert_eq!(result.kind, "function");
        assert_eq!(result.id.as_deref(), Some("app::main"));
        assert_eq!(result.repo.as_deref(), Some("app"));
        assert_eq!(result.signature.as_deref(), Some("fn main()"));
        assert_eq!(
            result.doc_comment.as_deref(),
            Some("Starts the application")
        );
        assert_eq!(result.role.as_deref(), Some("entry_point"));
        assert_eq!(result.snippet.as_deref(), Some("fn main() { helper(); }"));
        assert!(result.file.is_none());
        assert_eq!(result.calls, vec!["app::helper".to_string()]);
        assert!(result.called_by.is_empty());
    }

    #[test]
    fn compact_search_snippet_trims_flattens_and_truncates() {
        let snippet = format!(
            "\n    func load() {{\n        {}\n    }}\n",
            "work(); ".repeat(120)
        );

        let compact = compact_symbol_snippet(&snippet);

        assert!(!compact.chars().next().is_some_and(char::is_whitespace));
        assert!(!compact.contains('\n'));
        assert!(compact.starts_with("func load() { work();"));
        assert!(compact.ends_with("..."));
        assert!(compact.len() <= 604);
    }
}
