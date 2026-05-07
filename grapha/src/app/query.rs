use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, bail};
use serde::Serialize;

use crate::{
    AssetCommands, BriefOutputFormat, ColorMode, ConceptCommands, ContextOutputFormat,
    FlowCommands, L10nCommands, OriginTerminalFilter, QueryOutputFormat, RepoArchOutputFormat,
    RepoDoctorOutputFormat, RepoInferenceOutputFormat, RepoSmellsOutputFormat, SymbolCommands,
    annotations, assets, cache, changes, concepts, config, fields, history, inferred, localization,
    maintenance, query, render, search,
};

use super::index::{
    load_graph, load_graph_for_l10n, load_graph_for_l10n_usages, load_graph_uncached,
    open_search_index,
};

fn query_cache_key(parts: &[&str]) -> String {
    parts.join("\0")
}

fn kind_label(kind: grapha_core::graph::NodeKind) -> String {
    serde_json::to_string(&kind)
        .unwrap_or_else(|_| format!("{kind:?}"))
        .trim_matches('"')
        .to_string()
}

fn format_ambiguity_error(query: &str, candidates: &[query::QueryCandidate]) -> String {
    let mut message = format!("ambiguous query: {query}\n");
    for candidate in candidates {
        message.push_str(&format!(
            "  - {} [{}] in {} ({})\n",
            candidate.name,
            kind_label(candidate.kind),
            candidate.file,
            candidate
                .locator
                .as_deref()
                .unwrap_or(candidate.id.as_str())
        ));
    }
    message.push_str(&format!("hint: {}", query::ambiguity_hint()));
    message
}

fn resolve_query_result<T>(
    result: Result<T, query::QueryResolveError>,
    missing_label: &str,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(query::QueryResolveError::NotFound { query }) => {
            Err(anyhow!("{missing_label} not found: {query}"))
        }
        Err(query::QueryResolveError::Ambiguous { query, candidates }) => {
            Err(anyhow!(format_ambiguity_error(&query, &candidates)))
        }
        Err(query::QueryResolveError::NotFunction { hint }) => Err(anyhow!(hint)),
    }
}

fn resolve_field_set(fields_flag: &Option<String>, path: &Path) -> fields::FieldSet {
    match fields_flag {
        Some(f) => fields::FieldSet::parse(f),
        None => {
            let cfg = config::load_config(path);
            if cfg.output.default_fields.is_empty() {
                fields::FieldSet::default()
            } else {
                fields::FieldSet::from_config(&cfg.output.default_fields)
            }
        }
    }
}

fn resolve_search_field_set(fields_flag: &Option<String>, path: &Path) -> fields::FieldSet {
    match fields_flag {
        Some(_) => resolve_field_set(fields_flag, path),
        None => {
            let cfg = config::load_config(path);
            let field_set = if cfg.output.default_fields.is_empty() {
                fields::FieldSet::default().without_file()
            } else {
                fields::FieldSet::from_config(&cfg.output.default_fields)
            };
            field_set
                .with_id()
                .with_locator()
                .with_annotation()
                .with_doc_comment()
        }
    }
}

pub(crate) fn tree_render_options(color: ColorMode) -> render::RenderOptions {
    use std::io::IsTerminal;

    match color {
        ColorMode::Always => render::RenderOptions::color(),
        ColorMode::Never => render::RenderOptions::plain(),
        ColorMode::Auto => {
            if std::io::stdout().is_terminal() {
                render::RenderOptions::color()
            } else {
                render::RenderOptions::plain()
            }
        }
    }
}

fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn repo_smells_cache_key(
    module: Option<&str>,
    file: Option<&str>,
    symbol: Option<&str>,
    format: RepoSmellsOutputFormat,
) -> String {
    let (scope, value) = if let Some(module) = module {
        ("module", module)
    } else if let Some(file) = file {
        ("file", file)
    } else if let Some(symbol) = symbol {
        ("symbol", symbol)
    } else {
        ("all", "")
    };

    query_cache_key(&["repo", "smells", format.as_str(), scope, value])
}

fn print_query_result<T, R>(
    result: &T,
    format: QueryOutputFormat,
    render_options: render::RenderOptions,
    tree_renderer: R,
) -> anyhow::Result<()>
where
    T: Serialize,
    R: FnOnce(&T, render::RenderOptions) -> String,
{
    match format {
        QueryOutputFormat::Json => print_json(result),
        QueryOutputFormat::Tree => {
            println!("{}", tree_renderer(result, render_options));
            Ok(())
        }
    }
}

fn handle_resolved_graph_query<T, Q, R>(
    path: &Path,
    format: QueryOutputFormat,
    render_options: render::RenderOptions,
    missing_label: &str,
    query_fn: Q,
    tree_renderer: R,
) -> anyhow::Result<()>
where
    T: Serialize,
    Q: FnOnce(&grapha_core::graph::Graph) -> Result<T, query::QueryResolveError>,
    R: FnOnce(&T, render::RenderOptions) -> String,
{
    let graph = load_graph(path)?;
    let result = resolve_query_result(query_fn(&graph), missing_label)?;
    print_query_result(&result, format, render_options, tree_renderer)
}

fn handle_graph_query<T, Q, R>(
    path: &Path,
    format: QueryOutputFormat,
    render_options: render::RenderOptions,
    query_fn: Q,
    tree_renderer: R,
) -> anyhow::Result<()>
where
    T: Serialize,
    Q: FnOnce(&grapha_core::graph::Graph) -> T,
    R: FnOnce(&T, render::RenderOptions) -> String,
{
    let graph = load_graph(path)?;
    let result = query_fn(&graph);
    print_query_result(&result, format, render_options, tree_renderer)
}

pub(crate) fn handle_symbol_command(
    command: SymbolCommands,
    render_options: render::RenderOptions,
) -> anyhow::Result<()> {
    match command {
        SymbolCommands::Search {
            query,
            limit,
            path,
            kind,
            module,
            repo,
            file,
            role,
            fuzzy,
            exact_name,
            declarations_only,
            public_only,
            context,
            fields,
        } => {
            let field_set = resolve_search_field_set(&fields, &path);
            let index = open_search_index(&path)?;
            let options = search::SearchOptions {
                kind,
                module,
                repo,
                file_glob: file,
                role,
                fuzzy,
                exact_name,
                declarations_only,
                public_only,
            };
            let t = Instant::now();
            let results = search::search_filtered(&index, &query, limit, &options)?;
            let elapsed = t.elapsed();
            let graph = if search::needs_graph_for_projection(field_set, context) {
                Some(load_graph(&path)?)
            } else {
                None
            };
            let annotations = if field_set.annotation {
                Some(annotations::AnnotationStore::for_project_root(&path).load_index()?)
            } else {
                None
            };
            let projected = search::project_results(
                &results,
                graph.as_ref(),
                field_set,
                context,
                annotations.as_ref(),
            );
            print_json(&projected)?;
            if let Ok(status) = crate::index_status::load_index_status(&path, &path.join(".grapha"))
                && (status.freshness_tracking_available || status.temporary)
                && status.may_be_stale
            {
                let reason = status.note.unwrap_or_else(|| {
                    format!(
                        "{} indexed input file(s) changed since last index",
                        status.changed_input_file_count_since_index
                    )
                });
                eprintln!("  \x1b[33m!\x1b[0m results may be stale ({reason})");
            }

            eprintln!(
                "\n  {} results in {:.1}ms",
                results.len(),
                elapsed.as_secs_f64() * 1000.0,
            );
            Ok(())
        }
        SymbolCommands::Context {
            symbol,
            path,
            format,
            fields,
            limit,
        } => {
            let field_set = resolve_field_set(&fields, &path);
            let render_options = render_options.with_fields(field_set);
            let graph = load_graph(&path)?;
            let context_options = query::context::ContextQueryOptions { limit };
            let mut result = resolve_query_result(
                query::context::query_context_with_options(&graph, &symbol, &context_options),
                "symbol",
            )?;
            if field_set.annotation {
                let annotations =
                    annotations::AnnotationStore::for_project_root(&path).load_index()?;
                result.apply_annotations(&graph, &annotations);
            }

            match format {
                ContextOutputFormat::Json => print_json(&result),
                ContextOutputFormat::Tree => {
                    println!(
                        "{}",
                        render::render_context_with_options(&result, render_options)
                    );
                    Ok(())
                }
                ContextOutputFormat::Brief => {
                    println!(
                        "{}",
                        render::render_context_brief_with_options(&result, render_options)
                    );
                    Ok(())
                }
            }
        }
        SymbolCommands::Impact {
            symbol,
            depth,
            path,
            format,
            fields,
            limit,
        } => {
            let field_set = resolve_field_set(&fields, &path);
            let render_options = render_options.with_fields(field_set);
            let graph = load_graph(&path)?;
            let impact_options = query::impact::ImpactQueryOptions { limit };
            let result = resolve_query_result(
                query::impact::query_impact_with_options(&graph, &symbol, depth, &impact_options),
                "symbol",
            )?;
            match format {
                BriefOutputFormat::Json => print_json(&result),
                BriefOutputFormat::Tree => {
                    println!(
                        "{}",
                        render::render_impact_with_options(&result, render_options)
                    );
                    Ok(())
                }
                BriefOutputFormat::Brief => {
                    println!(
                        "{}",
                        render::render_impact_brief_with_options(&result, render_options)
                    );
                    Ok(())
                }
            }
        }
        SymbolCommands::Complexity { symbol, path } => {
            let graph = load_graph(&path)?;
            let result =
                query::complexity::query_complexity(&graph, &symbol).map_err(|e| anyhow!("{e}"))?;
            print_json(&result)
        }
        SymbolCommands::File { file, path } => {
            let graph = load_graph(&path)?;
            let result = query::file_symbols::query_file_symbols(&graph, &file);
            if result.total == 0 {
                anyhow::bail!("no symbols found in file matching: {file}");
            }
            print_json(&result)
        }
        SymbolCommands::Annotate {
            symbol,
            annotation,
            by,
            path,
        } => {
            let graph = load_graph(&path)?;
            let node = resolve_query_result(query::resolve_node(&graph, &symbol), "symbol")?;
            let result = annotations::AnnotationStore::for_project_root(&path).upsert_for_node(
                node,
                &annotation,
                by.as_deref(),
            )?;
            cache::QueryCache::new(&path.join(".grapha")).invalidate();
            print_json(&result)
        }
        SymbolCommands::Annotation { symbol, path } => {
            let graph = load_graph(&path)?;
            let node = resolve_query_result(query::resolve_node(&graph, &symbol), "symbol")?;
            let Some(result) =
                annotations::AnnotationStore::for_project_root(&path).get_for_node(node)?
            else {
                bail!("no annotation stored for symbol: {symbol}");
            };
            print_json(&result)
        }
    }
}

pub(crate) fn handle_flow_command(
    command: FlowCommands,
    render_options: render::RenderOptions,
) -> anyhow::Result<()> {
    match command {
        FlowCommands::Trace {
            symbol,
            direction,
            depth,
            path,
            format,
            fields,
            limit,
        } => match direction {
            crate::TraceDirection::Forward => {
                let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
                let graph = load_graph(&path)?;
                let trace_options = query::trace::TraceQueryOptions { limit };
                let result = resolve_query_result(
                    query::trace::query_trace_with_options(
                        &graph,
                        &symbol,
                        depth.unwrap_or(10),
                        &trace_options,
                    ),
                    "symbol",
                )?;
                match format {
                    BriefOutputFormat::Json => print_json(&result),
                    BriefOutputFormat::Tree => {
                        println!(
                            "{}",
                            render::render_trace_with_options(&result, render_options)
                        );
                        Ok(())
                    }
                    BriefOutputFormat::Brief => {
                        println!(
                            "{}",
                            render::render_trace_brief_with_options(&result, render_options)
                        );
                        Ok(())
                    }
                }
            }
            crate::TraceDirection::Reverse => {
                let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
                let graph = load_graph(&path)?;
                let reverse_options = query::reverse::ReverseQueryOptions { limit };
                let result = resolve_query_result(
                    query::reverse::query_reverse_with_options(
                        &graph,
                        &symbol,
                        depth,
                        &reverse_options,
                    ),
                    "symbol",
                )?;
                match format {
                    BriefOutputFormat::Json => print_json(&result),
                    BriefOutputFormat::Tree => {
                        println!(
                            "{}",
                            render::render_reverse_with_options(&result, render_options)
                        );
                        Ok(())
                    }
                    BriefOutputFormat::Brief => {
                        println!(
                            "{}",
                            render::render_reverse_brief_with_options(&result, render_options)
                        );
                        Ok(())
                    }
                }
            }
        },
        FlowCommands::Graph {
            symbol,
            depth,
            path,
            format,
            fields,
            limit,
        } => {
            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            let dataflow_options = query::dataflow::DataflowQueryOptions { limit };
            handle_resolved_graph_query(
                &path,
                format,
                render_options,
                "symbol",
                |graph| {
                    query::dataflow::query_dataflow_with_options(
                        graph,
                        &symbol,
                        depth,
                        &dataflow_options,
                    )
                },
                render::render_dataflow_with_options,
            )
        }
        FlowCommands::Origin {
            symbol,
            depth,
            terminal_kind,
            path,
            format,
            fields,
            limit,
        } => {
            let field_set = resolve_field_set(&fields, &path);
            let render_options = render_options.with_fields(field_set);
            let origin_options = query::origin::OriginQueryOptions { limit };
            handle_resolved_graph_query(
                &path,
                format,
                render_options,
                "symbol",
                |graph| {
                    let result = query::origin::query_origin_with_path_and_options(
                        graph,
                        &symbol,
                        depth,
                        Some(&path),
                        &origin_options,
                    )?;
                    // Order matters: filter first so `--limit` operates on the
                    // user-visible, post-filter origin set; otherwise
                    // `--terminal-kind=network --limit=20` could return < 20
                    // network origins because non-network origins were
                    // truncated out first.
                    let result = query::origin::filter_origin_result_by_terminal_kind(
                        result,
                        terminal_kind.map(OriginTerminalFilter::as_str),
                    );
                    let result = query::origin::apply_origin_limit(result, &origin_options);
                    Ok(query::origin::project_origin_result(result, field_set))
                },
                render::render_origin_with_options,
            )
        }
        FlowCommands::Entries {
            path,
            module,
            file,
            limit,
            format,
            fields,
        } => {
            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            handle_graph_query(
                &path,
                format,
                render_options,
                move |graph| {
                    query::entries::query_entries_with_options(
                        graph,
                        &query::entries::EntriesQueryOptions {
                            module,
                            file,
                            limit: Some(limit),
                        },
                    )
                },
                render::render_entries_with_options,
            )
        }
    }
}

pub(crate) fn handle_l10n_command(
    command: L10nCommands,
    render_options: render::RenderOptions,
) -> anyhow::Result<()> {
    match command {
        L10nCommands::Symbol {
            symbol,
            path,
            format,
            fields,
        } => {
            let store_dir = path.join(".grapha");
            let db_path = store_dir.join("grapha.db");
            let query_cache = cache::QueryCache::new(&store_dir);
            let format_key = format!("{format:?}");
            let fields_key = fields.as_deref().unwrap_or("");
            let cache_key = query_cache_key(&["l10n", "symbol", &symbol, &format_key, fields_key]);

            if let Some(cached) = query_cache.get(&cache_key, &db_path) {
                print!("{cached}");
                return Ok(());
            }

            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            let graph = load_graph_for_l10n(&path)?;
            let catalogs = localization::load_catalog_index(&path)?;
            let result = resolve_query_result(
                query::localize::query_localize(&graph, &catalogs, &symbol),
                "symbol",
            )?;

            let output = match format {
                QueryOutputFormat::Json => {
                    let s = serde_json::to_string_pretty(&result)?;
                    println!("{s}");
                    format!("{s}\n")
                }
                QueryOutputFormat::Tree => {
                    let s = render::render_localize_with_options(&result, render_options);
                    println!("{s}");
                    format!("{s}\n")
                }
            };
            let _ = query_cache.put(&cache_key, &db_path, &output);
            Ok(())
        }
        L10nCommands::Usages {
            key,
            table,
            path,
            format,
            fields,
        } => {
            let store_dir = path.join(".grapha");
            let db_path = store_dir.join("grapha.db");
            let query_cache = cache::QueryCache::new(&store_dir);
            let format_key = format!("{format:?}");
            let table_key = table.as_deref().unwrap_or("");
            let fields_key = fields.as_deref().unwrap_or("");
            let cache_key =
                query_cache_key(&["l10n", "usages", &key, table_key, &format_key, fields_key]);

            if let Some(cached) = query_cache.get(&cache_key, &db_path) {
                print!("{cached}");
                return Ok(());
            }

            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            let graph = load_graph_for_l10n_usages(&path)?;
            let catalogs = localization::load_catalog_index(&path)?;
            let result = query::usages::query_usages(&graph, &catalogs, &key, table.as_deref());

            let output = match format {
                QueryOutputFormat::Json => {
                    let s = serde_json::to_string_pretty(&result)?;
                    println!("{s}");
                    format!("{s}\n")
                }
                QueryOutputFormat::Tree => {
                    let s = render::render_usages_with_options(&result, render_options);
                    println!("{s}");
                    format!("{s}\n")
                }
            };
            let _ = query_cache.put(&cache_key, &db_path, &output);
            Ok(())
        }
    }
}

pub(crate) fn handle_asset_command(
    command: AssetCommands,
    render_options: render::RenderOptions,
) -> anyhow::Result<()> {
    match command {
        AssetCommands::List { unused, path } => {
            if unused {
                let graph = load_graph(&path)?;
                let index = assets::load_asset_index(&path)?;
                let unused = assets::find_unused(&index, &graph);
                print_json(&unused)
            } else {
                let index = assets::load_asset_index(&path)?;
                let records = index.all_records().to_vec();
                print_json(&records)
            }
        }
        AssetCommands::Usages {
            name,
            path,
            format,
            fields,
        } => {
            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            let graph = load_graph(&path)?;
            let usages = assets::find_usages(&graph, &name);
            match format {
                QueryOutputFormat::Json => print_json(&usages),
                QueryOutputFormat::Tree => {
                    if usages.is_empty() {
                        eprintln!("  no usages found for asset '{name}'");
                    } else {
                        for usage in &usages {
                            let file_label = if render_options.fields.file {
                                format!(" ({})", usage.file)
                            } else {
                                String::new()
                            };
                            println!("  {}{} — {}", usage.node_name, file_label, usage.asset_name);
                        }
                    }
                    Ok(())
                }
            }
        }
    }
}

pub(crate) fn handle_concept_command(
    command: ConceptCommands,
    render_options: render::RenderOptions,
) -> anyhow::Result<()> {
    match command {
        ConceptCommands::Search {
            term,
            limit,
            path,
            format,
            fields,
        } => {
            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            let graph = load_graph(&path)?;
            let search_index = open_search_index(&path)?;
            let concept_index = concepts::load_concept_index(&path)?;
            let catalogs = localization::load_catalog_index(&path).unwrap_or_default();
            let assets_index = assets::load_asset_index(&path).unwrap_or_default();
            let annotations = annotations::AnnotationStore::for_project_root(&path)
                .load_index()
                .ok();
            let result = concepts::search_concepts_with_annotations(
                &graph,
                &search_index,
                &concept_index,
                &catalogs,
                &assets_index,
                &term,
                limit,
                annotations.as_ref(),
            )?;
            print_query_result(
                &result,
                format,
                render_options,
                render::render_concept_search_with_options,
            )
        }
        ConceptCommands::Show {
            term,
            path,
            format,
            fields,
        } => {
            let render_options = render_options.with_fields(resolve_field_set(&fields, &path));
            let graph = load_graph(&path)?;
            let concept_index = concepts::load_concept_index(&path)?;
            let result = concepts::show_concept(&graph, &concept_index, &term)?;
            print_query_result(
                &result,
                format,
                render_options,
                render::render_concept_show_with_options,
            )
        }
        ConceptCommands::Bind {
            term,
            symbols,
            path,
        } => {
            let graph = load_graph(&path)?;
            let mut concept_index = concepts::load_concept_index(&path)?;
            let mut unique_ids = std::collections::BTreeSet::new();

            for symbol in symbols {
                let node = resolve_query_result(query::resolve_node(&graph, &symbol), "symbol")?;
                unique_ids.insert(node.id.clone());
            }

            let result = concept_index.bind_concept(
                &term,
                &unique_ids.into_iter().collect::<Vec<_>>(),
                vec![concepts::ConceptEvidence {
                    kind: "manual".to_string(),
                    value: term.trim().to_string(),
                    match_kind: "confirmed".to_string(),
                    table: None,
                    key: None,
                    source_value: None,
                    ui_path: Vec::new(),
                    note: Some("manual concept binding".to_string()),
                }],
            )?;
            concepts::save_concept_index(&path, &concept_index)?;
            print_json(&result)
        }
        ConceptCommands::Alias {
            term,
            aliases,
            path,
        } => {
            let mut concept_index = concepts::load_concept_index(&path)?;
            let result = concept_index.add_aliases(&term, &aliases)?;
            concepts::save_concept_index(&path, &concept_index)?;
            print_json(&result)
        }
        ConceptCommands::Remove { term, path } => {
            let mut concept_index = concepts::load_concept_index(&path)?;
            let result = concept_index.remove_concept(&term);
            concepts::save_concept_index(&path, &concept_index)?;
            print_json(&result)
        }
        ConceptCommands::Prune { path } => {
            let graph = load_graph(&path)?;
            let mut concept_index = concepts::load_concept_index(&path)?;
            let valid_ids: std::collections::HashSet<&str> =
                graph.nodes.iter().map(|node| node.id.as_str()).collect();
            let result = concept_index.prune(&valid_ids);
            concepts::save_concept_index(&path, &concept_index)?;
            print_json(&result)
        }
    }
}

pub(crate) fn handle_repo_command(command: crate::RepoCommands) -> anyhow::Result<()> {
    match command {
        crate::RepoCommands::Status { path } => {
            let status = crate::index_status::load_index_status(&path, &path.join(".grapha"))?;
            print_json(&status)
        }
        crate::RepoCommands::Changes { scope, path, limit } => {
            let graph = load_graph(&path)?;
            let change_options = changes::ChangeQueryOptions { limit };
            let report =
                changes::detect_changes_with_options(&path, &graph, &scope, &change_options)?;
            print_json(&report)
        }
        crate::RepoCommands::Map { module, path } => {
            let graph = load_graph(&path)?;
            let map = query::map::file_map(&graph, module.as_deref());
            print_json(&map)
        }
        crate::RepoCommands::Arch { path, format } => {
            let graph = load_graph(&path)?;
            let cfg = config::load_config(&path);
            let result = query::arch::check_architecture(&graph, &cfg.architecture);
            match format {
                RepoArchOutputFormat::Json => print_json(&result),
                RepoArchOutputFormat::Brief => {
                    println!(
                        "{}",
                        render::render_architecture_brief_with_options(&result)
                    );
                    Ok(())
                }
            }
        }
        crate::RepoCommands::Smells {
            module,
            file,
            symbol,
            no_cache,
            format,
            path,
        } => {
            let selected_scope_count = usize::from(module.is_some())
                + usize::from(file.is_some())
                + usize::from(symbol.is_some());
            if selected_scope_count > 1 {
                bail!("choose only one of --module, --file, or --symbol");
            }

            let store_dir = path.join(".grapha");
            let db_path = store_dir.join("grapha.db");
            let query_cache = cache::QueryCache::new(&store_dir);
            let cache_key = repo_smells_cache_key(
                module.as_deref(),
                file.as_deref(),
                symbol.as_deref(),
                format,
            );

            if !no_cache && let Some(cached) = query_cache.get(&cache_key, &db_path) {
                print!("{cached}");
                return Ok(());
            }

            let graph = if no_cache {
                load_graph_uncached(&path)?
            } else {
                load_graph(&path)?
            };
            let result = if let Some(ref file_query) = file {
                query::smells::detect_smells_for_file(&graph, file_query)
            } else if let Some(ref symbol_query) = symbol {
                let node =
                    resolve_query_result(query::resolve_node(&graph, symbol_query), "symbol")?;
                query::smells::detect_smells_for_symbol(&graph, &node.id)
            } else if let Some(ref module_name) = module {
                query::smells::detect_smells_for_module(&graph, module_name)
            } else {
                query::smells::detect_smells(&graph)
            };

            let output = match format {
                RepoSmellsOutputFormat::Json => {
                    format!("{}\n", serde_json::to_string_pretty(&result)?)
                }
                RepoSmellsOutputFormat::Brief => {
                    format!("{}\n", render::render_smells_brief_with_options(&result))
                }
            };

            if no_cache {
                print!("{output}");
                Ok(())
            } else {
                print!("{output}");
                let _ = query_cache.put(&cache_key, &db_path, &output);
                Ok(())
            }
        }
        crate::RepoCommands::Modules { path } => {
            let graph = load_graph(&path)?;
            let result = query::module_summary::query_module_summary(&graph);
            print_json(&result)
        }
        crate::RepoCommands::Infer { format, path } => {
            let cfg = config::load_config(&path);
            let store_path = inferred::inferred_store_path(&path);
            let (index, saved) = if cfg.inferred.enabled {
                let graph = load_graph(&path)?;
                let index = inferred::build_inferred_index(&graph);
                inferred::save_inferred_index(&path, &index)?;
                (index, true)
            } else {
                (inferred::load_inferred_index(&path)?, false)
            };
            let result = inferred::build_result(cfg.inferred.enabled, saved, &store_path, &index);
            match format {
                RepoInferenceOutputFormat::Json => print_json(&result),
                RepoInferenceOutputFormat::Brief => {
                    println!("{}", render::render_inferred_brief_with_options(&result));
                    Ok(())
                }
            }
        }
        crate::RepoCommands::Doctor { format, path } => {
            let graph = load_graph(&path)?;
            let inferred = inferred::load_inferred_index(&path)?;
            let report = maintenance::run_maintenance_checks(&graph, &inferred);
            match format {
                RepoDoctorOutputFormat::Json => print_json(&report),
                RepoDoctorOutputFormat::Brief => {
                    println!("{}", render::render_maintenance_brief_with_options(&report));
                    Ok(())
                }
            }
        }
        crate::RepoCommands::History { command } => handle_history_command(command),
    }
}

fn handle_history_command(command: crate::HistoryCommands) -> anyhow::Result<()> {
    match command {
        crate::HistoryCommands::Add {
            kind,
            title,
            at,
            status,
            commit,
            branch,
            detail,
            files,
            modules,
            symbols,
            metadata,
            path,
        } => {
            let symbols = resolve_history_symbols(&path, symbols)?;
            let event =
                history::HistoryStore::for_project(&path).add(history::NewHistoryEvent {
                    kind: kind.into(),
                    timestamp: at,
                    title,
                    status,
                    commit,
                    branch,
                    detail,
                    files,
                    modules,
                    symbols,
                    metadata: parse_history_metadata(metadata)?,
                })?;
            print_json(&event)
        }
        crate::HistoryCommands::List {
            kind,
            file,
            module,
            symbol,
            limit,
            path,
        } => {
            let symbol = match symbol {
                Some(symbol) => Some(resolve_history_symbols(&path, vec![symbol])?.remove(0)),
                None => None,
            };
            let events =
                history::HistoryStore::for_project(&path).list(&history::HistoryListFilter {
                    kind: kind.map(Into::into),
                    file,
                    module,
                    symbol,
                    limit,
                })?;
            print_json(&events)
        }
    }
}

fn resolve_history_symbols(path: &Path, symbols: Vec<String>) -> anyhow::Result<Vec<String>> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    let graph = load_graph(path)?;
    symbols
        .into_iter()
        .map(|symbol| {
            resolve_query_result(query::resolve_node(&graph, &symbol), "symbol")
                .map(|node| node.id.clone())
        })
        .collect()
}

fn parse_history_metadata(values: Vec<String>) -> anyhow::Result<BTreeMap<String, String>> {
    let mut metadata = BTreeMap::new();
    for value in values {
        let Some((key, val)) = value.split_once('=') else {
            bail!("metadata must be formatted as key=value");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("metadata key cannot be empty");
        }
        metadata.insert(key.to_string(), val.trim().to_string());
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::resolve_search_field_set;

    #[test]
    fn default_symbol_search_fields_include_doc_comment() {
        let project = tempfile::tempdir().unwrap();

        let fields = resolve_search_field_set(&None, project.path());

        assert!(fields.id);
        assert!(fields.locator);
        assert!(fields.annotation);
        assert!(fields.doc_comment);
        assert!(!fields.file);
        assert!(!fields.score);
    }

    #[test]
    fn explicit_symbol_search_fields_do_not_force_doc_comment() {
        let project = tempfile::tempdir().unwrap();

        let fields = resolve_search_field_set(&Some("id".to_string()), project.path());

        assert!(fields.id);
        assert!(!fields.doc_comment);
        assert!(!fields.annotation);
    }
}
