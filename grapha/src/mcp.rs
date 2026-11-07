pub mod handler;
pub mod types;

use std::io::{self, BufRead, Write};

use serde_json::json;

use handler::McpState;
use types::{JsonRpcRequest, JsonRpcResponse};

/// A loaded graph is a cache, not session state. Keep recall across eviction,
/// and reload the persisted snapshot on the next tool call.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn evict_index(state: &mut McpState) {
    if state.needs_index {
        return;
    }
    state.graph = grapha_core::graph::Graph::new();
    state.search_index = tantivy::Index::create_in_ram(tantivy::schema::Schema::builder().build());
    state.freshness = None;
    state.needs_index = true;
}

/// Watch notifications carry no graph; source changes are coalesced until queried.
pub fn run_mcp_server_with_watch(
    state: McpState,
    watch_rx: std::sync::mpsc::Receiver<()>,
    verbose: bool,
) -> anyhow::Result<()> {
    run_server(state, Some(watch_rx), verbose)
}

pub fn run_mcp_server(state: McpState) -> anyhow::Result<()> {
    run_server(state, None, false)
}

fn run_server(
    mut state: McpState,
    watch_rx: Option<std::sync::mpsc::Receiver<()>>,
    verbose: bool,
) -> anyhow::Result<()> {
    // A bounded input queue lets the state owner evict an idle graph even while
    // stdin is blocked, without letting a fast client queue unlimited requests.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("grapha-mcp-stdin".into())
        .spawn(move || {
            for line in io::stdin().lock().lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
    serve_requests(
        &mut state,
        watch_rx,
        rx,
        &mut io::stdout(),
        IDLE_TIMEOUT,
        verbose,
    )
}

fn serve_requests(
    state: &mut McpState,
    watch_rx: Option<std::sync::mpsc::Receiver<()>>,
    rx: std::sync::mpsc::Receiver<io::Result<String>>,
    stdout: &mut impl Write,
    idle_timeout: std::time::Duration,
    verbose: bool,
) -> anyhow::Result<()> {
    let mut last_tool_call = std::time::Instant::now();
    let mut needs_reindex = false;
    loop {
        if !state.needs_index && last_tool_call.elapsed() >= idle_timeout {
            evict_index(state);
        }
        let timeout = if state.needs_index {
            idle_timeout
        } else {
            idle_timeout.saturating_sub(last_tool_call.elapsed())
        };
        let line = match rx.recv_timeout(timeout) {
            Ok(Ok(line)) => line,
            Ok(Err(error)) => {
                eprintln!("mcp: failed to read stdin: {error}");
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                evict_index(state);
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(updates) = watch_rx.as_ref() {
            let mut changed = false;
            while updates.try_recv().is_ok() {
                changed = true;
            }
            if changed {
                needs_reindex = true;
                evict_index(state);
                if verbose {
                    eprintln!("watch: sources changed; will reindex on next tool call");
                }
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    stdout,
                    &JsonRpcResponse::error(
                        serde_json::Value::Null,
                        -32700,
                        format!("parse error: {error}"),
                    ),
                )?;
                continue;
            }
        };
        let Some(id) = request.id else { continue };
        if request.method == "tools/call" && needs_reindex {
            if let Err(error) = handler::rebuild_index(state) {
                write_response(stdout, &JsonRpcResponse::success(id, error))?;
                continue;
            }
            needs_reindex = false;
        }
        let response = dispatch(state, id, &request.method, &request.params);
        if request.method == "tools/call" {
            last_tool_call = std::time::Instant::now();
        }
        write_response(stdout, &response)?;
    }
    Ok(())
}

fn dispatch(
    state: &mut McpState,
    id: serde_json::Value,
    method: &str,
    params: &serde_json::Value,
) -> JsonRpcResponse {
    match method {
        "initialize" => JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "grapha", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "tools/list" => {
            let tools = handler::tool_definitions();
            JsonRpcResponse::success(id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            let result = handler::handle_tool_call(state, tool_name, &arguments);
            JsonRpcResponse::success(id, result)
        }
        _ => JsonRpcResponse::error(id, -32601, format!("method not found: {method}")),
    }
}

fn write_response(stdout: &mut impl Write, response: &JsonRpcResponse) -> anyhow::Result<()> {
    let json = serde_json::to_string(response)?;
    stdout.write_all(json.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use grapha_core::graph::Graph;
    use tantivy::Index;
    use tantivy::schema::Schema;

    fn make_test_state() -> McpState {
        let graph = Graph {
            version: String::new(),
            nodes: vec![],
            edges: vec![],
        };
        let schema = Schema::builder().build();
        let index = Index::create_in_ram(schema);
        McpState {
            graph,
            search_index: index,
            project_root: PathBuf::from("/tmp/project"),
            store_path: PathBuf::from("/tmp/test"),
            recall: crate::recall::Recall::new(),
            needs_index: false,
            freshness: None,
        }
    }

    #[test]
    fn idle_eviction_reloads_persisted_graph_without_reindexing() {
        use crate::store::Store;
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join(".grapha");
        std::fs::create_dir_all(&store_path).unwrap();
        let graph = Graph {
            version: "persisted-test".into(),
            nodes: vec![],
            edges: vec![],
        };
        crate::store::sqlite::SqliteStore::new(store_path.join("grapha.db"))
            .save(&graph)
            .unwrap();
        let mut state = make_test_state();
        state.project_root = dir.path().into();
        state.store_path = store_path;

        evict_index(&mut state);
        assert!(state.needs_index);
        assert!(state.freshness.is_none());
        handler::ensure_indexed(&mut state).unwrap();
        assert_eq!(state.graph, graph);
        assert!(!state.needs_index);
        assert_eq!(
            crate::store::sqlite::SqliteStore::new(state.store_path.join("grapha.db"))
                .load()
                .unwrap(),
            graph
        );

        evict_index(&mut state);
        evict_index(&mut state);
        handler::ensure_indexed(&mut state).unwrap();
        assert_eq!(state.graph, graph);
    }

    #[test]
    fn initialization_and_tool_listing_leave_graph_unloaded() {
        let mut state = make_test_state();
        evict_index(&mut state);
        for method in ["initialize", "tools/list"] {
            assert!(
                dispatch(&mut state, json!(1), method, &json!({}))
                    .error
                    .is_none()
            );
            assert!(state.needs_index);
            assert!(state.graph.nodes.is_empty());
        }
    }

    #[test]
    fn idle_timer_runs_without_input_and_ignores_initialization_traffic() {
        let mut state = make_test_state();
        let (tx, rx) = std::sync::mpsc::channel();
        let sender = std::thread::spawn(move || {
            // Keep stdin connected while the idle timer fires.
            for _ in 0..5 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                tx.send(Ok(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}).to_string(),
                ))
                .unwrap();
            }
        });
        let mut output = Vec::new();
        serve_requests(
            &mut state,
            None,
            rx,
            &mut output,
            std::time::Duration::from_millis(5),
            false,
        )
        .unwrap();
        sender.join().unwrap();
        assert!(state.needs_index);
        assert_eq!(String::from_utf8(output).unwrap().lines().count(), 5);
    }

    #[test]
    fn watch_changes_are_coalesced_and_rebuilt_on_tool_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn changed_symbol() {}\n").unwrap();
        let mut state = make_test_state();
        state.project_root = dir.path().into();
        state.store_path = dir.path().join(".grapha");
        let (watch_tx, watch_rx) = std::sync::mpsc::sync_channel(1);
        watch_tx.try_send(()).unwrap();
        for _ in 0..1000 {
            assert!(matches!(
                watch_tx.try_send(()),
                Err(std::sync::mpsc::TrySendError::Full(()))
            ));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "get_file_map", "arguments": {}
            }})
            .to_string(),
        ))
        .unwrap();
        drop(tx);
        let mut output = Vec::new();
        serve_requests(
            &mut state,
            Some(watch_rx),
            rx,
            &mut output,
            IDLE_TIMEOUT,
            false,
        )
        .unwrap();
        assert!(!state.needs_index, "{}", String::from_utf8_lossy(&output));
        assert!(state.graph.nodes.iter().any(|n| n.name == "changed_symbol"));
        let response: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_ne!(response["result"]["isError"], true);
    }

    #[test]
    fn dispatch_initialize() {
        let mut state = make_test_state();
        let resp = dispatch(&mut state, json!(1), "initialize", &json!({}));
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "grapha");
    }

    #[test]
    fn dispatch_tools_list() {
        let mut state = make_test_state();
        let resp = dispatch(&mut state, json!(2), "tools/list", &json!({}));
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), handler::tool_definitions().len());
    }

    #[test]
    fn dispatch_unknown_method() {
        let mut state = make_test_state();
        let resp = dispatch(&mut state, json!(3), "bogus/method", &json!({}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn dispatch_tools_call() {
        let mut state = make_test_state();
        let params = json!({
            "name": "get_file_map",
            "arguments": {}
        });
        let resp = dispatch(&mut state, json!(4), "tools/call", &params);
        let result = resp.result.unwrap();
        assert!(result["content"].is_array());
    }
}
