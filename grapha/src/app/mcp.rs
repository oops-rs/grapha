use std::path::PathBuf;

use anyhow::Context;

use crate::{index_status, mcp, recall, watch};

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
    let store_path = path.join(".grapha");
    // A client may initialize many workspace MCPs without ever querying them.
    // Do not materialize a graph until the first tool call.
    let state = empty_state_for_lazy_index(path.clone(), store_path.clone());

    if options.watch {
        run_mcp_with_watch(path, state, verbose)
    } else {
        mcp::run_mcp_server(state)
    }
}

pub(crate) fn build_initial_state(
    path: &std::path::Path,
    store_path: &std::path::Path,
    verbose: bool,
) -> anyhow::Result<mcp::handler::McpState> {
    if !store_path.join("grapha.db").exists() {
        anyhow::bail!(
            "no index database at {}",
            store_path.join("grapha.db").display()
        );
    }

    let freshness_before = index_status::capture_index_freshness(path, store_path)
        .context("failed to capture index freshness")?;
    let graph = load_graph(path)?;
    let search_index = open_search_index(path, verbose)?;
    let freshness_after = index_status::capture_index_freshness(path, store_path)
        .context("failed to capture index freshness")?;
    let freshness = (freshness_before.snapshot_identity() == freshness_after.snapshot_identity())
        .then_some(freshness_after);
    Ok(mcp::handler::McpState {
        graph,
        search_index,
        project_root: path.to_path_buf(),
        store_path: store_path.to_path_buf(),
        recall: recall::Recall::new(),
        needs_index: false,
        freshness,
    })
}

fn empty_state_for_lazy_index(
    project_root: PathBuf,
    store_path: PathBuf,
) -> mcp::handler::McpState {
    let schema = tantivy::schema::Schema::builder().build();
    let search_index = tantivy::Index::create_in_ram(schema);
    mcp::handler::McpState {
        graph: grapha_core::graph::Graph {
            version: String::new(),
            nodes: vec![],
            edges: vec![],
        },
        search_index,
        project_root,
        store_path,
        recall: recall::Recall::new(),
        needs_index: true,
        freshness: None,
    }
}

fn run_mcp_with_watch(
    path: PathBuf,
    state: mcp::handler::McpState,
    verbose: bool,
) -> anyhow::Result<()> {
    let (rx, _guard) =
        watch::start_watcher(&path, &["swift", "rs", "ts", "tsx", "js", "jsx", "vue"])?;
    // Watch events invalidate the graph; extraction waits until a tool call.
    // Idle clients must not build/retain workspace-sized snapshots in parallel.
    let (state_tx, state_rx) = std::sync::mpsc::sync_channel::<()>(1);
    std::thread::Builder::new()
        .name("grapha-watch-invalidate".into())
        .spawn(move || {
            for _event in rx {
                match state_tx.try_send(()) {
                    Ok(()) | Err(std::sync::mpsc::TrySendError::Full(())) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(())) => break,
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
