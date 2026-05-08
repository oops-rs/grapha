use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use grapha_core::Classifier;

use crate::{
    cache, classify, compress, config, filter, polyglot_plugin, progress, rust_plugin, snippet,
};

pub(crate) struct PipelineOutput {
    pub(crate) graph: grapha_core::graph::Graph,
    pub(crate) extraction_cache_entries:
        std::collections::HashMap<String, cache::ExtractionCacheEntry>,
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

fn default_repo_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("local")
        .to_string()
}

fn primary_repo_name(path: &Path, cfg: &config::GraphaConfig) -> String {
    cfg.repo
        .name
        .clone()
        .unwrap_or_else(|| default_repo_name(path))
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

fn stamp_repo(
    mut result: grapha_core::ExtractionResult,
    repo_name: &str,
    namespace_ids: bool,
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
pub(crate) fn run_pipeline(
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
    for ext in &cfg.external {
        let ext_path = Path::new(&ext.path);
        if !ext_path.exists() {
            if verbose {
                eprintln!(
                    "  \x1b[33m!\x1b[0m external repo '{}' not found at {}, skipping",
                    ext.name, ext.path
                );
            }
            continue;
        }
        let mut ext_context = grapha_core::project_context(ext_path);
        ext_context.index_store_enabled = cfg.swift.index_store;
        match grapha_core::pipeline::discover_files(ext_path, &registry) {
            Ok(ext_discovered) => {
                indexed_files.extend(ext_discovered.into_iter().map(|file| IndexedInputFile {
                    path: file,
                    repo_name: ext.name.clone(),
                    context: ext_context.clone(),
                }));
                external_repo_count += 1;
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
    for ext in &cfg.external {
        let ext_path = Path::new(&ext.path);
        if !ext_path.exists() {
            continue;
        }
        let mut ext_context = grapha_core::project_context(ext_path);
        ext_context.index_store_enabled = cfg.swift.index_store;
        if !ext_context.index_store_enabled {
            grapha_swift::clear_index_store_path(&ext_context.project_root);
        }
        if let Ok(ext_modules) = grapha_core::discover_modules(&registry, &ext_context) {
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

    let results: Vec<_> = indexed_files
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

pub(crate) fn handle_analyze(
    path: PathBuf,
    output: Option<PathBuf>,
    filter: Option<String>,
    compact: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let mut graph = run_pipeline(&path, verbose, false, None)?.graph;

    if let Some(ref filter_str) = filter {
        let kinds = filter::parse_filter(filter_str)?;
        graph = filter::filter_graph(graph, &kinds);
    }

    let json = if compact {
        let pruned = compress::prune::prune(graph, false);
        let grouped = compress::group::group(&pruned);
        match &output {
            Some(_) => serde_json::to_string(&grouped)?,
            None => serde_json::to_string_pretty(&grouped)?,
        }
    } else {
        match &output {
            Some(_) => serde_json::to_string(&graph)?,
            None => serde_json::to_string_pretty(&graph)?,
        }
    };

    match output {
        Some(p) => {
            std::fs::write(&p, &json)
                .with_context(|| format!("failed to write {}", p.display()))?;
            if verbose {
                eprintln!("  \x1b[32m✓\x1b[0m wrote {}", p.display());
            }
        }
        None => println!("{json}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_pipeline, stamp_repo};
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

        let stamped = stamp_repo(result, "shared", true);

        assert_eq!(stamped.nodes[0].id, "shared::src/main.rs::load");
        assert_eq!(stamped.nodes[0].repo.as_deref(), Some("shared"));
        assert_eq!(stamped.edges[0].source, "shared::src/main.rs::load");
        assert_eq!(stamped.edges[0].target, "shared::src/main.rs::save");
        assert_eq!(stamped.edges[0].repo.as_deref(), Some("shared"));
        assert_eq!(
            stamped.edges[0].provenance[0].symbol_id,
            "shared::src/main.rs::load"
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
