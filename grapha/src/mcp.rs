pub mod handler;
pub mod types;

use std::io::{self, BufRead, Write};

use serde_json::json;

use handler::McpState;
use types::{JsonRpcRequest, JsonRpcResponse};

/// Run MCP server with a watch channel that hot-swaps the graph on file changes.
pub fn run_mcp_server_with_watch(
    mut state: McpState,
    watch_rx: std::sync::mpsc::Receiver<(grapha_core::graph::Graph, tantivy::Index)>,
    verbose: bool,
) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Set stdin to non-blocking-ish: use lines() but poll watch_rx between requests
    for line in stdin.lock().lines() {
        // Check for graph updates from watcher (non-blocking)
        while let Ok((graph, index)) = watch_rx.try_recv() {
            let node_count = graph.nodes.len();
            let edge_count = graph.edges.len();
            state.graph = graph;
            state.search_index = index;

            let valid_ids: std::collections::HashSet<&str> =
                state.graph.nodes.iter().map(|n| n.id.as_str()).collect();
            state.recall.prune(&valid_ids);

            if verbose {
                eprintln!("watch: graph updated ({node_count} nodes, {edge_count} edges)");
            }
        }

        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("mcp: failed to read stdin: {e}");
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    -32700,
                    format!("parse error: {e}"),
                );
                write_response(&mut stdout, &resp)?;
                continue;
            }
        };

        let id = match request.id {
            Some(id) => id,
            None => continue,
        };

        let response = dispatch(&mut state, id, &request.method, &request.params);
        write_response(&mut stdout, &response)?;
    }

    Ok(())
}

pub fn run_mcp_server(mut state: McpState) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("mcp: failed to read stdin: {e}");
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    -32700,
                    format!("parse error: {e}"),
                );
                write_response(&mut stdout, &resp)?;
                continue;
            }
        };

        // Notifications have no id -- skip without responding
        let id = match request.id {
            Some(id) => id,
            None => continue,
        };

        let response = dispatch(&mut state, id, &request.method, &request.params);
        write_response(&mut stdout, &response)?;
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

fn write_response(stdout: &mut io::Stdout, response: &JsonRpcResponse) -> anyhow::Result<()> {
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
        }
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
