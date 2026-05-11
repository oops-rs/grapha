use std::path::PathBuf;

use crate::store::Store;
use crate::{index_status, mcp, recall, search, store, watch};

use super::index::{load_graph, open_search_index};

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpOptions {
    watch: bool,
}

fn resolve_mcp_options(path: &std::path::Path, cli_watch: Option<bool>) -> McpOptions {
    let project = crate::config::load_config(path).serve;
    let global = crate::config::load_global_config().serve;
    resolve_mcp_options_from(cli_watch, project, global)
}

fn resolve_mcp_options_from(
    cli_watch: Option<bool>,
    project: crate::config::ServeConfig,
    global: crate::config::ServeConfig,
) -> McpOptions {
    let watch = cli_watch
        .or(project.watch)
        .or(global.watch)
        .unwrap_or(false);

    McpOptions { watch }
}

pub(crate) fn handle_mcp(
    path: PathBuf,
    watch_mode: Option<bool>,
    verbose: bool,
) -> anyhow::Result<()> {
    let options = resolve_mcp_options(&path, watch_mode);
    let graph = load_graph(&path)?;
    let search_index = open_search_index(&path, verbose)?;
    let state = mcp::handler::McpState {
        graph,
        search_index,
        project_root: path.clone(),
        store_path: path.join(".grapha"),
        recall: recall::Recall::new(),
    };

    if options.watch {
        run_mcp_with_watch(path, state, verbose)
    } else {
        mcp::run_mcp_server(state)
    }
}

fn run_mcp_with_watch(
    path: PathBuf,
    state: mcp::handler::McpState,
    verbose: bool,
) -> anyhow::Result<()> {
    let (rx, _guard) =
        watch::start_watcher(&path, &["swift", "rs", "ts", "tsx", "js", "jsx", "vue"])?;
    let store_path = path.join(".grapha");
    let project_path = path.clone();

    let (state_tx, state_rx) =
        std::sync::mpsc::channel::<(grapha_core::graph::Graph, tantivy::Index)>();

    std::thread::Builder::new()
        .name("grapha-watch-reindex".into())
        .spawn(move || {
            for event in rx {
                match event {
                    watch::WatchEvent::FilesChanged(files) => {
                        if verbose {
                            eprintln!("watch: {} file(s) changed, re-indexing...", files.len());
                        }
                        match crate::app::pipeline::run_pipeline(
                            &project_path,
                            verbose,
                            false,
                            None,
                        ) {
                            Ok(output) => {
                                let graph = output.graph;
                                let store_file = store_path.join("grapha.db");
                                let store = store::sqlite::SqliteStore::new(store_file);
                                if let Err(e) = store.save(&graph) {
                                    eprintln!("watch: failed to save graph: {e}");
                                    continue;
                                }
                                let search_path = store_path.join("search_index");
                                match search::build_index(&graph, &search_path) {
                                    Ok(index) => {
                                        if let Err(e) = index_status::save_index_status(
                                            &project_path,
                                            &store_path,
                                            graph.nodes.len(),
                                            graph.edges.len(),
                                            &crate::config::load_config(&project_path),
                                        ) {
                                            eprintln!("watch: failed to save index status: {e}");
                                            continue;
                                        }
                                        if state_tx.send((graph, index)).is_err() {
                                            break;
                                        }
                                        if verbose {
                                            eprintln!("watch: re-index complete");
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("watch: failed to build search index: {e}");
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("watch: re-index failed: {e}");
                            }
                        }
                    }
                }
            }
        })?;

    mcp::run_mcp_server_with_watch(state, state_rx, verbose)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_config(
        host: Option<&str>,
        port: Option<u16>,
        watch: Option<bool>,
    ) -> crate::config::ServeConfig {
        crate::config::ServeConfig {
            host: host.map(str::to_string),
            port,
            watch,
        }
    }

    #[test]
    fn mcp_options_prefer_cli_watch() {
        let options = resolve_mcp_options_from(
            Some(false),
            serve_config(None, None, Some(true)),
            serve_config(None, None, Some(true)),
        );

        assert_eq!(options, McpOptions { watch: false });
    }

    #[test]
    fn mcp_options_use_project_watch_before_global() {
        let options = resolve_mcp_options_from(
            None,
            serve_config(None, None, Some(true)),
            serve_config(None, None, Some(false)),
        );

        assert_eq!(options, McpOptions { watch: true });
    }

    #[test]
    fn mcp_options_fall_back_to_global_then_default() {
        assert_eq!(
            resolve_mcp_options_from(
                None,
                serve_config(Some("ignored"), Some(1), None),
                serve_config(None, None, Some(true)),
            ),
            McpOptions { watch: true }
        );
        assert_eq!(
            resolve_mcp_options_from(
                None,
                serve_config(None, None, None),
                serve_config(None, None, None),
            ),
            McpOptions { watch: false }
        );
    }
}
