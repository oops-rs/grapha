use std::path::PathBuf;

use crate::store::Store;
use crate::{index_status, mcp, recall, search, serve, store, watch};

use super::index::{load_graph, open_search_index};

const DEFAULT_SERVE_HOST: &str = "0.0.0.0";
const DEFAULT_SERVE_PORT: u16 = 8080;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeOptions {
    host: String,
    port: u16,
    watch: bool,
}

fn resolve_serve_options(
    path: &std::path::Path,
    cli_host: Option<String>,
    cli_port: Option<u16>,
    cli_watch: Option<bool>,
) -> ServeOptions {
    let project = crate::config::load_config(path).serve;
    let global = crate::config::load_global_config().serve;
    resolve_serve_options_from(cli_host, cli_port, cli_watch, project, global)
}

fn resolve_serve_options_from(
    cli_host: Option<String>,
    cli_port: Option<u16>,
    cli_watch: Option<bool>,
    project: crate::config::ServeConfig,
    global: crate::config::ServeConfig,
) -> ServeOptions {
    let host = cli_host
        .and_then(non_empty)
        .or_else(|| project.host.and_then(non_empty))
        .or_else(|| global.host.and_then(non_empty))
        .unwrap_or_else(|| DEFAULT_SERVE_HOST.to_string());
    let port = cli_port
        .or(project.port)
        .or(global.port)
        .unwrap_or(DEFAULT_SERVE_PORT);
    let watch = cli_watch
        .or(project.watch)
        .or(global.watch)
        .unwrap_or(false);

    ServeOptions { host, port, watch }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn run_mcp_server_with_optional_watch(
    path: PathBuf,
    graph: grapha_core::graph::Graph,
    search_index: tantivy::Index,
    watch_mode: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let state = mcp::handler::McpState {
        graph,
        search_index,
        project_root: path.clone(),
        store_path: path.join(".grapha"),
        recall: recall::Recall::new(),
    };

    let _watcher_guard = if watch_mode {
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
                                                eprintln!(
                                                    "watch: failed to save index status: {e}"
                                                );
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

        mcp::run_mcp_server_with_watch(state, state_rx, verbose)?;
        return Ok(());
    } else {
        None::<watch::WatcherGuard>
    };

    mcp::run_mcp_server(state)
}

pub(crate) fn handle_serve(
    path: PathBuf,
    host: Option<String>,
    port: Option<u16>,
    mcp_mode: bool,
    watch_mode: Option<bool>,
    verbose: bool,
) -> anyhow::Result<()> {
    let options = resolve_serve_options(&path, host, port, watch_mode);
    let graph = load_graph(&path)?;
    let search_index = open_search_index(&path, verbose)?;

    if mcp_mode {
        run_mcp_server_with_optional_watch(path, graph, search_index, options.watch, verbose)
    } else {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(serve::run(
            path,
            graph,
            search_index,
            options.host,
            options.port,
        ))?;
        Ok(())
    }
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
    fn serve_options_prefer_cli_over_project_and_global() {
        let options = resolve_serve_options_from(
            Some(" 127.0.0.1 ".to_string()),
            Some(19090),
            Some(false),
            serve_config(Some("0.0.0.0"), Some(18081), Some(true)),
            serve_config(Some("localhost"), Some(18080), Some(true)),
        );

        assert_eq!(
            options,
            ServeOptions {
                host: "127.0.0.1".to_string(),
                port: 19090,
                watch: false
            }
        );
    }

    #[test]
    fn serve_options_prefer_project_over_global() {
        let options = resolve_serve_options_from(
            None,
            None,
            None,
            serve_config(Some("127.0.0.1"), Some(18081), Some(true)),
            serve_config(Some("0.0.0.0"), Some(18080), Some(false)),
        );

        assert_eq!(
            options,
            ServeOptions {
                host: "127.0.0.1".to_string(),
                port: 18081,
                watch: true
            }
        );
    }

    #[test]
    fn serve_options_fall_back_to_global_then_defaults() {
        let options = resolve_serve_options_from(
            Some(" ".to_string()),
            None,
            None,
            serve_config(Some("\t"), None, None),
            serve_config(None, Some(18080), Some(true)),
        );

        assert_eq!(
            options,
            ServeOptions {
                host: DEFAULT_SERVE_HOST.to_string(),
                port: 18080,
                watch: true
            }
        );
    }
}
