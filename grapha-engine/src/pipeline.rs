use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use grapha_core::Classifier;

use crate::store::Store;
use crate::{
    cache, cargo_manifest, classify, config, http_client, polyglot_plugin, progress, remote,
    rust_plugin, snippet, store,
};

pub struct PipelineOutput {
    pub graph: grapha_core::graph::Graph,
    pub extraction_cache_entries: std::collections::HashMap<String, cache::ExtractionCacheEntry>,
}

fn builtin_registry() -> anyhow::Result<grapha_core::LanguageRegistry> {
    let mut registry = grapha_core::LanguageRegistry::new();
    rust_plugin::register_builtin(&mut registry)?;
    grapha_swift::register_builtin(&mut registry)?;
    polyglot_plugin::register_builtin(&mut registry)?;
    Ok(registry)
}

#[derive(Clone)]
struct IndexedInputFile {
    path: PathBuf,
    repo_name: String,
    context: grapha_core::ProjectContext,
}

fn primary_repo_name(path: &Path, cfg: &config::GraphaConfig) -> String {
    cfg.repo
        .name
        .clone()
        .unwrap_or_else(|| crate::data_paths::repo_name_for_project_root(path))
}

fn extraction_cache_key(repo_name: &str, path: &Path) -> String {
    format!("{repo_name}\0{}", path.to_string_lossy())
}

fn make_extraction_cache_entry(
    file: &Path,
    repo_name: &str,
    file_context: &grapha_core::FileContext,
    config_fingerprint: &str,
    result: &grapha_core::ExtractionResult,
) -> Option<(String, cache::ExtractionCacheEntry)> {
    let stamp = cache::FileStamp::from_path(file)?;
    Some((
        extraction_cache_key(repo_name, &file_context.relative_path),
        cache::ExtractionCacheEntry {
            stamp,
            module_name: file_context.module_name.clone(),
            config_fingerprint: config_fingerprint.to_string(),
            result: result.clone(),
        },
    ))
}

fn repo_scoped_id(repo_name: &str, id: &str) -> String {
    format!("{repo_name}::{id}")
}

#[derive(Debug, Clone)]
struct EvidenceMetadata {
    source: &'static str,
    channel: Option<String>,
    head_oid: Option<String>,
    head_ref: Option<String>,
}

impl EvidenceMetadata {
    fn local_precise() -> Self {
        Self {
            source: "local_precise",
            channel: None,
            head_oid: None,
            head_ref: None,
        }
    }

    fn remote_baseline(metadata: &remote::ProjectRevisionMetadata) -> Self {
        Self {
            source: "remote_baseline",
            channel: Some(metadata.channel.clone()),
            head_oid: metadata.head_oid.clone(),
            head_ref: metadata.head_ref.clone(),
        }
    }
}

fn stamp_repo(
    mut result: grapha_core::ExtractionResult,
    repo_name: &str,
    namespace_ids: bool,
    evidence: &EvidenceMetadata,
) -> grapha_core::ExtractionResult {
    let repo = repo_name.to_string();
    let id_map = namespace_ids.then(|| {
        result
            .nodes
            .iter()
            .map(|node| (node.id.clone(), repo_scoped_id(repo_name, &node.id)))
            .collect::<HashMap<_, _>>()
    });

    for node in &mut result.nodes {
        if let Some(id_map) = &id_map
            && let Some(scoped_id) = id_map.get(&node.id)
        {
            node.id = scoped_id.clone();
        }
        node.repo = Some(repo.clone());
        node.metadata.insert(
            "grapha.evidence.source".to_string(),
            evidence.source.to_string(),
        );
        if let Some(channel) = &evidence.channel {
            node.metadata
                .insert("grapha.evidence.channel".to_string(), channel.clone());
        }
        if let Some(head_oid) = &evidence.head_oid {
            node.metadata
                .insert("grapha.evidence.head_oid".to_string(), head_oid.clone());
        }
        if let Some(head_ref) = &evidence.head_ref {
            node.metadata
                .insert("grapha.evidence.head_ref".to_string(), head_ref.clone());
        }
    }
    for edge in &mut result.edges {
        if let Some(id_map) = &id_map {
            if let Some(scoped_source) = id_map.get(&edge.source) {
                edge.source = scoped_source.clone();
            }
            if let Some(scoped_target) = id_map.get(&edge.target) {
                edge.target = scoped_target.clone();
            }
            for provenance in &mut edge.provenance {
                if let Some(scoped_symbol_id) = id_map.get(&provenance.symbol_id) {
                    provenance.symbol_id = scoped_symbol_id.clone();
                }
            }
        }
        edge.repo = Some(repo.clone());
    }
    result
}

fn graph_to_extraction_result(graph: grapha_core::graph::Graph) -> grapha_core::ExtractionResult {
    grapha_core::ExtractionResult {
        nodes: graph.nodes,
        edges: graph.edges,
        imports: Vec::new(),
    }
}

fn load_graph_from_store_dir(store_dir: &Path) -> anyhow::Result<grapha_core::graph::Graph> {
    let sqlite_path = store_dir.join("grapha.db");
    if sqlite_path.exists() {
        return store::sqlite::SqliteStore::new(sqlite_path).load();
    }
    let json_path = store_dir.join("graph.json");
    if json_path.exists() {
        return store::json::JsonStore::new(json_path).load();
    }
    anyhow::bail!("no Grapha store found at {}", store_dir.display())
}

fn candidate_external_store_dirs(ext: &config::ExternalRepo) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(index_path) = ext.index_path.as_deref().and_then(non_empty) {
        let path = PathBuf::from(index_path);
        dirs.push(if is_store_dir(&path) {
            path
        } else {
            path.join(".grapha")
        });
    }
    if let Some(path) = ext.path.as_deref().and_then(non_empty) {
        let path = PathBuf::from(path);
        dirs.push(if is_store_dir(&path) {
            path
        } else {
            path.join(".grapha")
        });
    }
    dirs
}

fn is_store_dir(path: &Path) -> bool {
    path.join("grapha.db").exists() || path.join("graph.json").exists()
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn load_external_index_result(
    ext: &config::ExternalRepo,
) -> anyhow::Result<Option<grapha_core::ExtractionResult>> {
    for store_dir in candidate_external_store_dirs(ext) {
        if !is_store_dir(&store_dir) {
            continue;
        }
        let graph = load_graph_from_store_dir(&store_dir)?;
        let result = stamp_repo(
            graph_to_extraction_result(graph),
            &ext.name,
            true,
            &EvidenceMetadata::local_precise(),
        );
        return Ok(Some(result));
    }
    Ok(None)
}

fn load_external_remote_result(
    ext: &config::ExternalRepo,
) -> anyhow::Result<Option<grapha_core::ExtractionResult>> {
    let Some(remote_cfg) = ext.remote.as_ref() else {
        return Ok(None);
    };
    let channel = non_empty(&remote_cfg.channel).unwrap_or(remote::DEFAULT_CHANNEL);
    let bundle = if let Some(server) = remote_cfg.server.as_deref().and_then(non_empty) {
        let endpoint = http_client::HttpEndpoint::parse(server)?;
        let project_id = urlencoding::encode(&remote_cfg.project_id);
        let channel = urlencoding::encode(channel);
        http_client::get_json(
            &endpoint,
            &format!("/api/projects/{project_id}/revision?channel={channel}"),
        )?
    } else {
        remote::ProjectRevisionStore::new(crate::data_paths::global_data_root())
            .load_bundle(&remote_cfg.project_id, channel)?
    };
    let evidence = EvidenceMetadata::remote_baseline(&bundle.metadata);
    Ok(Some(stamp_repo(
        graph_to_extraction_result(bundle.graph),
        &ext.name,
        true,
        &evidence,
    )))
}

fn external_source_path(ext: &config::ExternalRepo) -> Option<PathBuf> {
    ext.path
        .as_deref()
        .and_then(non_empty)
        .map(PathBuf::from)
        .filter(|path| path.exists())
}

fn manifest_dependency_result(project_root: &Path) -> Option<grapha_core::ExtractionResult> {
    let result = cargo_manifest::extract_dependency_graph(project_root);
    (!result.nodes.is_empty() || !result.edges.is_empty()).then_some(result)
}

fn apply_config_classifier_semantics(
    document: &mut grapha_core::SemanticDocument,
    rules: &[config::ClassifierRule],
) {
    if rules.is_empty() {
        return;
    }

    let classifier = classify::toml_rules::TomlRulesClassifier::new(rules);
    document.override_call_relations(|relation, source| {
        let context = grapha_core::ClassifyContext {
            source_node: relation.source.clone(),
            file: source.map(|symbol| symbol.file.clone()).unwrap_or_default(),
            arguments: Vec::new(),
        };
        classifier
            .classify(relation.target.as_raw(), &context)
            .map(|classification| grapha_core::TerminalEffect {
                terminal_kind: classification.terminal_kind,
                direction: classification.direction,
                operation: classification.operation,
            })
    });
}

/// Run the extraction pipeline on a path, returning a merged graph.
pub fn run_pipeline(
    path: &Path,
    verbose: bool,
    timing: bool,
    existing_extraction_cache: Option<
        &std::collections::HashMap<String, cache::ExtractionCacheEntry>,
    >,
) -> anyhow::Result<PipelineOutput> {
    let t = Instant::now();
    let registry = builtin_registry()?;
    let mut project_context = grapha_core::project_context(path);

    let cfg = config::load_config(path);
    let config_fingerprint = cfg.extraction_cache_fingerprint();
    project_context.index_store_enabled = cfg.swift.index_store;
    let primary_repo = primary_repo_name(&project_context.project_root, &cfg);

    let (files, _) = std::thread::scope(|scope| {
        let files_handle = scope.spawn(|| {
            grapha_core::pipeline::discover_files(path, &registry)
                .context("failed to discover files")
        });
        let plugin_handle =
            scope.spawn(|| grapha_core::prepare_plugins(&registry, &project_context));
        let files = files_handle.join().expect("discover thread panicked")?;
        plugin_handle.join().expect("plugin thread panicked")?;
        Ok::<_, anyhow::Error>((files, ()))
    })?;

    let mut indexed_files: Vec<IndexedInputFile> = files
        .into_iter()
        .map(|file| IndexedInputFile {
            path: file,
            repo_name: primary_repo.clone(),
            context: project_context.clone(),
        })
        .collect();
    let primary_file_count = indexed_files.len();
    let mut external_repo_count = 0usize;
    let mut external_seed_results = Vec::new();
    if let Some(result) = manifest_dependency_result(&project_context.project_root) {
        external_seed_results.push(stamp_repo(
            result,
            &primary_repo,
            false,
            &EvidenceMetadata::local_precise(),
        ));
    }
    let mut external_source_contexts = Vec::new();
    for ext in &cfg.external {
        match load_external_index_result(ext) {
            Ok(Some(result)) => {
                external_seed_results.push(result);
                external_repo_count += 1;
                continue;
            }
            Ok(None) => {}
            Err(error) if verbose => {
                eprintln!(
                    "  \x1b[33m!\x1b[0m failed to load external index for '{}': {error}",
                    ext.name
                );
            }
            Err(_) => {}
        }

        let Some(ext_path) = external_source_path(ext) else {
            match load_external_remote_result(ext) {
                Ok(Some(result)) => {
                    external_seed_results.push(result);
                    external_repo_count += 1;
                }
                Ok(None) => {
                    if verbose {
                        eprintln!(
                            "  \x1b[33m!\x1b[0m external repo '{}' has no available local or remote evidence, skipping",
                            ext.name
                        );
                    }
                }
                Err(error) if verbose => {
                    eprintln!(
                        "  \x1b[33m!\x1b[0m failed to load remote baseline for '{}': {error}",
                        ext.name
                    );
                }
                Err(_) => {}
            }
            continue;
        };

        let mut ext_context = grapha_core::project_context(&ext_path);
        ext_context.index_store_enabled = cfg.swift.index_store;
        match grapha_core::pipeline::discover_files(&ext_path, &registry) {
            Ok(ext_discovered) => {
                if let Some(result) = manifest_dependency_result(&ext_path) {
                    external_seed_results.push(stamp_repo(
                        result,
                        &ext.name,
                        true,
                        &EvidenceMetadata::local_precise(),
                    ));
                }
                indexed_files.extend(ext_discovered.into_iter().map(|file| IndexedInputFile {
                    path: file,
                    repo_name: ext.name.clone(),
                    context: ext_context.clone(),
                }));
                external_repo_count += 1;
                external_source_contexts.push(ext_context);
            }
            Err(e) => {
                if verbose {
                    eprintln!(
                        "  \x1b[33m!\x1b[0m failed to discover files in '{}': {e}",
                        ext.name
                    );
                }
            }
        }
    }

    let external_file_count = indexed_files.len().saturating_sub(primary_file_count);

    if verbose {
        let msg = if external_file_count > 0 {
            format!(
                "discovered {} files + {} external ({} repos)",
                primary_file_count, external_file_count, external_repo_count
            )
        } else {
            format!("discovered {} files", indexed_files.len())
        };
        progress::done(&msg, t);
        if let Some(store) = grapha_swift::index_store_path(&project_context.project_root) {
            progress::done(&format!("index store: {}", store.display()), t);
        }
    }

    let mut module_map = grapha_core::discover_modules(&registry, &project_context)?;
    for ext_context in &external_source_contexts {
        if !ext_context.index_store_enabled {
            grapha_swift::clear_index_store_path(&ext_context.project_root);
        }
        if let Ok(ext_modules) = grapha_core::discover_modules(&registry, ext_context) {
            module_map.merge(ext_modules);
        }
    }

    let t = Instant::now();
    let pb = if verbose && indexed_files.len() > 1 {
        Some(progress::bar(indexed_files.len() as u64, "extracting"))
    } else {
        None
    };

    use rayon::prelude::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    let skipped = AtomicUsize::new(0);
    let extracted = AtomicUsize::new(0);
    let reused_cached = AtomicUsize::new(0);

    let t_read_ns = AtomicU64::new(0);
    let t_extract_ns = AtomicU64::new(0);
    let t_snippet_ns = AtomicU64::new(0);
    let t_file_context_ns = AtomicU64::new(0);
    let t_total_per_file_ns = AtomicU64::new(0);
    let t_max_single_file_ns = AtomicU64::new(0);
    let extraction_cache_entries = Mutex::new(std::collections::HashMap::new());

    let mut results: Vec<_> = indexed_files
        .par_iter()
        .filter_map(|input| {
            let file = &input.path;
            let t_file_start = Instant::now();
            let t_fc = Instant::now();
            let file_context = grapha_core::file_context(&input.context, &module_map, file);
            t_file_context_ns.fetch_add(t_fc.elapsed().as_nanos() as u64, Ordering::Relaxed);
            let cache_key = extraction_cache_key(&input.repo_name, &file_context.relative_path);
            if let Some(existing_cache) = existing_extraction_cache
                && let Some(entry) = existing_cache.get(&cache_key)
                && entry.module_name.as_deref() == file_context.module_name.as_deref()
                && entry.config_fingerprint == config_fingerprint
                && cache::FileStamp::from_path(file).is_some_and(|stamp| stamp == entry.stamp)
            {
                reused_cached.fetch_add(1, Ordering::Relaxed);
                if let Some(ref pb) = pb {
                    pb.inc(1);
                }
                extraction_cache_entries
                    .lock()
                    .expect("extraction cache mutex poisoned")
                    .insert(cache_key, entry.clone());
                let file_ns = t_file_start.elapsed().as_nanos() as u64;
                t_total_per_file_ns.fetch_add(file_ns, Ordering::Relaxed);
                t_max_single_file_ns.fetch_max(file_ns, Ordering::Relaxed);
                return Some(entry.result.clone());
            }

            let t0 = Instant::now();
            let source = match std::fs::read(file) {
                Ok(s) => s,
                Err(_) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    if let Some(ref pb) = pb {
                        pb.inc(1);
                    }
                    let file_ns = t_file_start.elapsed().as_nanos() as u64;
                    t_total_per_file_ns.fetch_add(file_ns, Ordering::Relaxed);
                    t_max_single_file_ns.fetch_max(file_ns, Ordering::Relaxed);
                    return None;
                }
            };
            t_read_ns.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);

            let t1 = Instant::now();
            let semantic_result =
                grapha_core::extract_semantics_with_registry(&registry, &source, &file_context);
            t_extract_ns.fetch_add(t1.elapsed().as_nanos() as u64, Ordering::Relaxed);

            if let Some(ref pb) = pb {
                pb.inc(1);
            }

            match semantic_result {
                Ok(mut document) => {
                    extracted.fetch_add(1, Ordering::Relaxed);
                    apply_config_classifier_semantics(&mut document, &cfg.classifiers);
                    let mut result = stamp_repo(
                        grapha_core::lower_semantics(document),
                        &input.repo_name,
                        input.repo_name != primary_repo,
                        &EvidenceMetadata::local_precise(),
                    );
                    let t2 = Instant::now();
                    if result
                        .nodes
                        .iter()
                        .any(|n| snippet::should_extract_snippet(n.kind))
                    {
                        let source_str: std::borrow::Cow<'_, str> =
                            match std::str::from_utf8(&source) {
                                Ok(s) => std::borrow::Cow::Borrowed(s),
                                Err(_) => String::from_utf8_lossy(&source),
                            };
                        let line_idx = snippet::LineIndex::new(&source_str);
                        for node in &mut result.nodes {
                            if snippet::should_extract_snippet(node.kind) {
                                node.snippet = line_idx
                                    .extract_symbol_snippet(&node.span, &node.name, node.kind);
                            }
                        }
                    }
                    t_snippet_ns.fetch_add(t2.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    if let Some((key, entry)) = make_extraction_cache_entry(
                        file,
                        &input.repo_name,
                        &file_context,
                        &config_fingerprint,
                        &result,
                    ) {
                        extraction_cache_entries
                            .lock()
                            .expect("extraction cache mutex poisoned")
                            .insert(key, entry);
                    }
                    let file_ns = t_file_start.elapsed().as_nanos() as u64;
                    t_total_per_file_ns.fetch_add(file_ns, Ordering::Relaxed);
                    t_max_single_file_ns.fetch_max(file_ns, Ordering::Relaxed);
                    Some(result)
                }
                Err(e) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    if verbose && let Some(ref pb) = pb {
                        pb.suspend(|| {
                            eprintln!("  \x1b[33m!\x1b[0m skipping {}: {e}", file.display())
                        });
                    }
                    let file_ns = t_file_start.elapsed().as_nanos() as u64;
                    t_total_per_file_ns.fetch_add(file_ns, Ordering::Relaxed);
                    t_max_single_file_ns.fetch_max(file_ns, Ordering::Relaxed);
                    None
                }
            }
        })
        .collect();
    results.extend(external_seed_results);

    grapha_core::finish_plugins(&registry, &project_context)?;

    let skipped = skipped.load(Ordering::Relaxed);
    let extracted = extracted.load(Ordering::Relaxed);
    let reused_cached = reused_cached.load(Ordering::Relaxed);
    let extraction_cache_entries = extraction_cache_entries
        .into_inner()
        .expect("extraction cache mutex poisoned");

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    if timing {
        let read_ms = t_read_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let extract_ms = t_extract_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let snippet_ms = t_snippet_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let is_ms = grapha_swift::TIMING_INDEXSTORE_NS.load(std::sync::atomic::Ordering::Relaxed)
            as f64
            / 1_000_000.0;
        let ts_parse_ms = grapha_swift::TIMING_TS_PARSE_NS
            .load(std::sync::atomic::Ordering::Relaxed) as f64
            / 1_000_000.0;
        let doc_ms = grapha_swift::TIMING_TS_DOC_NS.load(std::sync::atomic::Ordering::Relaxed)
            as f64
            / 1_000_000.0;
        let swiftui_ms = grapha_swift::TIMING_TS_SWIFTUI_NS
            .load(std::sync::atomic::Ordering::Relaxed) as f64
            / 1_000_000.0;
        let l10n_ms = grapha_swift::TIMING_TS_L10N_NS.load(std::sync::atomic::Ordering::Relaxed)
            as f64
            / 1_000_000.0;
        let asset_ms = grapha_swift::TIMING_TS_ASSET_NS.load(std::sync::atomic::Ordering::Relaxed)
            as f64
            / 1_000_000.0;
        let ss_ms = grapha_swift::TIMING_SWIFTSYNTAX_NS.load(std::sync::atomic::Ordering::Relaxed)
            as f64
            / 1_000_000.0;
        let ts_fb_ms = grapha_swift::TIMING_TS_FALLBACK_NS
            .load(std::sync::atomic::Ordering::Relaxed) as f64
            / 1_000_000.0;
        let fc_ms = t_file_context_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let total_per_file_ms = t_total_per_file_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let max_single_file_ms = t_max_single_file_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        eprintln!(
            "    thread-summed: read {:.0}ms, extract {:.0}ms, snippet {:.0}ms, file_context {:.0}ms, total_per_file {:.0}ms",
            read_ms, extract_ms, snippet_ms, fc_ms, total_per_file_ms
        );
        eprintln!("    max_single_file: {:.0}ms", max_single_file_ms);
        eprintln!(
            "    swift: indexstore {:.0}ms, ts-parse {:.0}ms, doc {:.0}ms, swiftui {:.0}ms, l10n {:.0}ms, asset {:.0}ms, swiftsyntax {:.0}ms, ts-fallback {:.0}ms",
            is_ms, ts_parse_ms, doc_ms, swiftui_ms, l10n_ms, asset_ms, ss_ms, ts_fb_ms
        );
    }
    if verbose {
        let msg = if skipped > 0 && reused_cached > 0 {
            format!(
                "extracted {} files, reused {} cached extraction results ({} skipped)",
                extracted, reused_cached, skipped
            )
        } else if skipped > 0 {
            format!("extracted {} files ({} skipped)", extracted, skipped)
        } else if reused_cached > 0 {
            format!(
                "extracted {} files, reused {} cached extraction results",
                extracted, reused_cached
            )
        } else {
            format!("extracted {} files", extracted)
        };
        progress::done(&msg, t);
    }

    let t = Instant::now();
    let merged = grapha_core::merge(results);
    if verbose {
        progress::done(
            &format!(
                "merged → {} nodes, {} edges",
                merged.nodes.len(),
                merged.edges.len()
            ),
            t,
        );
    }

    let t = Instant::now();
    let graph = grapha_core::normalize_graph(merged);
    if verbose {
        let terminal_count = graph
            .nodes
            .iter()
            .filter(|n| matches!(n.role, Some(grapha_core::graph::NodeRole::Terminal { .. })))
            .count();
        let entry_count = graph
            .nodes
            .iter()
            .filter(|n| matches!(n.role, Some(grapha_core::graph::NodeRole::EntryPoint)))
            .count();
        progress::done(
            &format!(
                "classified → {} entries, {} terminals",
                entry_count, terminal_count
            ),
            t,
        );
    }

    Ok(PipelineOutput {
        graph,
        extraction_cache_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        EvidenceMetadata, graph_to_extraction_result, load_external_index_result, run_pipeline,
        stamp_repo,
    };
    use crate::store::Store;
    use crate::{config, remote};
    use grapha_core::ExtractionResult;
    use grapha_core::graph::{
        Edge, EdgeKind, EdgeProvenance, FlowDirection, Node, NodeKind, NodeRole, Span,
        TerminalKind, Visibility,
    };
    use std::fs;
    use tempfile::TempDir;

    fn write_rust_project(project_root: &std::path::Path, config: &str, source: &str) {
        let src_dir = project_root.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(project_root.join("grapha.toml"), config).unwrap();
        fs::write(src_dir.join("main.rs"), source).unwrap();
    }

    fn test_node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: id.to_string(),
            file: "src/main.rs".into(),
            span: Span {
                start: [1, 0],
                end: [1, 4],
            },
            visibility: Visibility::Private,
            metadata: Default::default(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    fn test_graph_with_node(id: &str, name: &str) -> grapha_core::graph::Graph {
        grapha_core::graph::Graph {
            version: "0.1.0".to_string(),
            nodes: vec![Node {
                id: id.to_string(),
                kind: NodeKind::Struct,
                name: name.to_string(),
                file: "src/lib.rs".into(),
                span: Span {
                    start: [1, 0],
                    end: [1, 10],
                },
                visibility: Visibility::Public,
                metadata: Default::default(),
                role: None,
                signature: None,
                doc_comment: None,
                module: Some("FrameUI".to_string()),
                snippet: None,
                repo: None,
            }],
            edges: Vec::new(),
        }
    }

    #[test]
    fn stamp_repo_namespaces_external_ids_and_edges() {
        let result = ExtractionResult {
            nodes: vec![
                test_node("src/main.rs::load"),
                test_node("src/main.rs::save"),
            ],
            edges: vec![Edge {
                source: "src/main.rs::load".to_string(),
                target: "src/main.rs::save".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![EdgeProvenance {
                    file: "src/main.rs".into(),
                    span: Span {
                        start: [1, 0],
                        end: [1, 4],
                    },
                    symbol_id: "src/main.rs::load".to_string(),
                }],
                repo: None,
            }],
            imports: Vec::new(),
        };

        let stamped = stamp_repo(result, "shared", true, &EvidenceMetadata::local_precise());

        assert_eq!(stamped.nodes[0].id, "shared::src/main.rs::load");
        assert_eq!(stamped.nodes[0].repo.as_deref(), Some("shared"));
        assert_eq!(
            stamped.nodes[0]
                .metadata
                .get("grapha.evidence.source")
                .map(String::as_str),
            Some("local_precise")
        );
        assert_eq!(stamped.edges[0].source, "shared::src/main.rs::load");
        assert_eq!(stamped.edges[0].target, "shared::src/main.rs::save");
        assert_eq!(stamped.edges[0].repo.as_deref(), Some("shared"));
        assert_eq!(
            stamped.edges[0].provenance[0].symbol_id,
            "shared::src/main.rs::load"
        );
    }

    #[test]
    fn external_index_result_keeps_repo_namespace_and_local_evidence() {
        let dir = TempDir::new().unwrap();
        let store_dir = dir.path().join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();
        crate::store::json::JsonStore::new(store_dir.join("graph.json"))
            .save(&test_graph_with_node(
                "src/lib.rs::GiftBanner",
                "GiftBanner",
            ))
            .unwrap();
        let external = config::ExternalRepo {
            name: "FrameUI".to_string(),
            path: None,
            index_path: Some(store_dir.to_string_lossy().to_string()),
            remote: None,
        };

        let result = load_external_index_result(&external).unwrap().unwrap();

        assert_eq!(result.nodes[0].id, "FrameUI::src/lib.rs::GiftBanner");
        assert_eq!(result.nodes[0].repo.as_deref(), Some("FrameUI"));
        assert_eq!(
            result.nodes[0]
                .metadata
                .get("grapha.evidence.source")
                .map(String::as_str),
            Some("local_precise")
        );
    }

    #[test]
    fn remote_baseline_stamp_keeps_repo_namespace_and_head_metadata() {
        let graph = test_graph_with_node("src/lib.rs::GiftBanner", "GiftBanner");
        let metadata = remote::ProjectRevisionMetadata {
            project_id: "remote-frameui".to_string(),
            repo_name: "FrameUI".to_string(),
            channel: "default".to_string(),
            head_oid: Some("1234567890abcdef".to_string()),
            head_ref: Some("main".to_string()),
            config_fingerprint: "{}".to_string(),
            graph_version: graph.version.clone(),
            grapha_version: "0.0.0-test".to_string(),
            bundle_schema_version: remote::PUBLISH_BUNDLE_SCHEMA_VERSION,
            published_at_unix_secs: 1,
        };
        let evidence = EvidenceMetadata::remote_baseline(&metadata);

        let result = stamp_repo(
            graph_to_extraction_result(graph),
            "FrameUI",
            true,
            &evidence,
        );

        assert_eq!(result.nodes[0].id, "FrameUI::src/lib.rs::GiftBanner");
        assert_eq!(
            result.nodes[0]
                .metadata
                .get("grapha.evidence.source")
                .map(String::as_str),
            Some("remote_baseline")
        );
        assert_eq!(
            result.nodes[0]
                .metadata
                .get("grapha.evidence.head_ref")
                .map(String::as_str),
            Some("main")
        );
    }

    #[test]
    fn run_pipeline_prefers_local_external_source_over_remote_baseline() {
        let project_dir = TempDir::new().unwrap();
        let app_root = project_dir.path().join("app");
        let external_root = project_dir.path().join("frameui");
        write_rust_project(
            &external_root,
            "",
            r#"
pub struct GiftBanner;
"#,
        );
        write_rust_project(
            &app_root,
            &format!(
                r#"
[[external]]
name = "FrameUI"
path = "{}"

[external.remote]
project_id = "missing-remote-frameui"
"#,
                external_root.display()
            ),
            r#"
fn main() {}
"#,
        );

        let output = run_pipeline(&app_root, false, false, None).unwrap();
        let node = output
            .graph
            .nodes
            .iter()
            .find(|node| node.name == "GiftBanner")
            .expect("expected external source symbol");

        assert_eq!(node.repo.as_deref(), Some("FrameUI"));
        assert_eq!(
            node.metadata
                .get("grapha.evidence.source")
                .map(String::as_str),
            Some("local_precise")
        );
    }

    #[test]
    fn run_pipeline_honors_swift_index_store_config_false_end_to_end() {
        let project_dir = TempDir::new().unwrap();
        let project_root = project_dir.path().join("MyApp");
        let source_dir = project_root.join("Sources");

        fs::create_dir_all(&source_dir).unwrap();
        fs::write(
            project_root.join("grapha.toml"),
            "[swift]\nindex_store = false\n",
        )
        .unwrap();
        fs::write(
            source_dir.join("ContentView.swift"),
            r#"
            import SwiftUI

            struct ContentView: View {
                var body: some View {
                    Text("Hello")
                }
            }
            "#,
        )
        .unwrap();

        let project_root = fs::canonicalize(&project_root).unwrap();

        grapha_swift::set_index_store_path(
            &project_root,
            Some(project_root.join("DerivedData/MyApp-abc123/Index.noindex/DataStore")),
        );

        let output = run_pipeline(&project_root, false, false, None).unwrap();

        assert!(
            output
                .graph
                .nodes
                .iter()
                .any(|node| node.name == "ContentView" && node.kind == NodeKind::Struct),
            "pipeline should still extract Swift symbols through fallback parsing"
        );
        assert!(
            grapha_swift::index_store_path(&project_root).is_none(),
            "cached index store should be cleared when [swift].index_store = false"
        );
    }

    #[test]
    fn run_pipeline_config_rules_override_builtin_terminal_effects() {
        let project_dir = TempDir::new().unwrap();
        let project_root = project_dir.path().join("demo");
        write_rust_project(
            &project_root,
            r#"
[[classifiers]]
pattern = "reqwest"
terminal = "event"
direction = "write"
operation = "CUSTOM_OVERRIDE"
"#,
            r#"
fn load() {
    reqwest::get("https://example.com");
}
"#,
        );

        let output = run_pipeline(&project_root, false, false, None).unwrap();
        let edge = output
            .graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Calls)
            .expect("expected a call edge");
        let load = output
            .graph
            .nodes
            .iter()
            .find(|node| node.name == "load")
            .expect("expected the load node");

        assert_eq!(edge.direction, Some(FlowDirection::Write));
        assert_eq!(edge.operation.as_deref(), Some("CUSTOM_OVERRIDE"));
        assert_eq!(
            load.role,
            Some(NodeRole::Terminal {
                kind: TerminalKind::Event
            })
        );
    }

    #[test]
    fn run_pipeline_invalidates_cached_results_when_classifier_rules_change() {
        let project_dir = TempDir::new().unwrap();
        let project_root = project_dir.path().join("demo");
        write_rust_project(
            &project_root,
            r#"
[[classifiers]]
pattern = "custom_api"
terminal = "network"
direction = "read"
operation = "FIRST_CFG"
"#,
            r#"
fn custom_api() {}

fn load() {
    custom_api();
}
"#,
        );

        let first = run_pipeline(&project_root, false, false, None).unwrap();

        fs::write(
            project_root.join("grapha.toml"),
            r#"
[[classifiers]]
pattern = "custom_api"
terminal = "event"
direction = "write"
operation = "SECOND_CFG"
"#,
        )
        .unwrap();

        let second = run_pipeline(
            &project_root,
            false,
            false,
            Some(&first.extraction_cache_entries),
        )
        .unwrap();

        let edge = second
            .graph
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Calls)
            .expect("expected a call edge");
        let custom_api = second
            .graph
            .nodes
            .iter()
            .find(|node| node.name == "custom_api")
            .expect("expected the custom_api node");

        assert_eq!(edge.direction, Some(FlowDirection::Write));
        assert_eq!(edge.operation.as_deref(), Some("SECOND_CFG"));
        assert_eq!(
            custom_api.role,
            Some(NodeRole::Terminal {
                kind: TerminalKind::Event
            })
        );
    }
}
