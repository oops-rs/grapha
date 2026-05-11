use std::path::PathBuf;

use grapha_core::graph::Graph;
use serde_json::{Value, json};
use tantivy::Index;

use crate::fields::FieldSet;
use crate::mcp::types::ToolDefinition;
use crate::query;
use crate::recall::{self, Recall};
use crate::search;
use crate::store::Store;
use crate::{annotations, assets, cluster, concepts, localization};

pub struct McpState {
    pub graph: Graph,
    pub search_index: Index,
    pub project_root: PathBuf,
    pub store_path: PathBuf,
    pub recall: Recall,
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "search_symbols".to_string(),
            description: "Search for symbols by name, kind, module, repo, file, or role. Returns matching symbols with relevance scores.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query (symbol name or keyword)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 20)",
                        "default": 20
                    },
                    "kind": {
                        "type": "string",
                        "description": "Filter by symbol kind (function, struct, enum, trait, etc.)"
                    },
                    "module": {
                        "type": "string",
                        "description": "Filter by module name"
                    },
                    "repo": {
                        "type": "string",
                        "description": "Filter by repo name"
                    },
                    "file": {
                        "type": "string",
                        "description": "Filter by file path glob"
                    },
                    "role": {
                        "type": "string",
                        "description": "Filter by role (entry_point, terminal, internal)"
                    },
                    "fuzzy": {
                        "type": "boolean",
                        "description": "Enable fuzzy matching (default: false)",
                        "default": false
                    },
                    "exact_name": {
                        "type": "boolean",
                        "description": "Require an exact declaration-name match (default: false)",
                        "default": false
                    },
                    "declarations_only": {
                        "type": "boolean",
                        "description": "Exclude synthetic nodes and accessor functions (default: false)",
                        "default": false
                    },
                    "public_only": {
                        "type": "boolean",
                        "description": "Keep only public symbols (default: false)",
                        "default": false
                    },
                    "fields": {
                        "type": "string",
                        "description": "Optional comma-separated projected fields to include (for example: score,id,locator,doc_comment,annotation,signature; or full/all/none)"
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters instead of the default list JSON",
                        "default": false
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "description": "1-based page within the selected cluster",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "description": "Items returned in the selected cluster page",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "description": "Candidates fetched before score-band clustering",
                        "default": 200
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "get_index_status".to_string(),
            description: "Show the last index timestamp, repo snapshot metadata, and whether results may be stale.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "get_symbol_context".to_string(),
            description: "Get 360-degree context for a symbol: callers, callees, implementors, containment, type references, and stored annotations when present.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name or ID"
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters for context list sections",
                        "default": false
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum items per context section when cluster is false (default: 20)",
                        "default": 20
                    },
                    "fields": {
                        "type": "string",
                        "description": "Optional comma-separated projected symbol fields to include (file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or full/all/none)"
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "default": 200
                    }
                },
                "required": ["symbol"]
            }),
        },
        ToolDefinition {
            name: "annotate_symbol".to_string(),
            description: "Attach or replace an agent-written annotation for a symbol, keyed by that symbol's Grapha ID/Swift USR.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name, locator, ID, or Swift USR"
                    },
                    "annotation": {
                        "type": "string",
                        "description": "Agent-written explanation of the symbol's usage or business role"
                    },
                    "created_by": {
                        "type": "string",
                        "description": "Optional agent or author label"
                    }
                },
                "required": ["symbol", "annotation"]
            }),
        },
        ToolDefinition {
            name: "get_impact".to_string(),
            description: "Analyze the blast radius of changing a symbol using BFS traversal.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name or ID"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Maximum traversal depth (default: 3)",
                        "default": 3
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum items per depth bucket when cluster is false (default: 20)",
                        "default": 20
                    },
                    "fields": {
                        "type": "string",
                        "description": "Optional comma-separated projected symbol fields to include (file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or full/all/none)"
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters for impact depth buckets",
                        "default": false
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "default": 200
                    }
                },
                "required": ["symbol"]
            }),
        },
        ToolDefinition {
            name: "get_file_map".to_string(),
            description: "Get a map of files and symbols organized by module and directory.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "module": {
                        "type": "string",
                        "description": "Filter by module name (optional)"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "trace".to_string(),
            description: "Trace dataflow forward from a symbol to terminals, or reverse from a symbol back to entry points.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name or ID"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["forward", "reverse"],
                        "description": "Trace direction (default: forward)",
                        "default": "forward"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Maximum traversal depth (default: 10 for forward, unlimited for reverse)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum forward flows or reverse entries when cluster is false (default: 20)",
                        "default": 20
                    },
                    "fields": {
                        "type": "string",
                        "description": "Optional comma-separated projected symbol fields to include (file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or full/all/none)"
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters for trace flows or reverse entries",
                        "default": false
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "default": 200
                    }
                },
                "required": ["symbol"]
            }),
        },
        // --- New tools ---
        ToolDefinition {
            name: "get_file_symbols".to_string(),
            description: "List all symbols in a file, ordered by source position. Returns declarations (structs, functions, properties, etc.) excluding synthetic view/branch nodes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "File name or path suffix (e.g. \"RoomPage.swift\" or \"src/main.rs\")"
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters for file symbols",
                        "default": false
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "default": 200
                    }
                },
                "required": ["file"]
            }),
        },
        ToolDefinition {
            name: "batch_context".to_string(),
            description: "Get 360-degree context for multiple symbols in a single call. Returns a map of symbol ID to context result. More efficient than multiple get_symbol_context calls.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbols": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Array of symbol names or IDs"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum items per context section for each symbol (default: 20)",
                        "default": 20
                    },
                    "fields": {
                        "type": "string",
                        "description": "Optional comma-separated projected symbol fields to include (file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or full/all/none)"
                    }
                },
                "required": ["symbols"]
            }),
        },
        ToolDefinition {
            name: "analyze_complexity".to_string(),
            description: "Analyze the structural complexity of a type (struct, class, enum, protocol). Returns property count, method count, dependency count, invalidation sources, init parameter count, extension count, containment depth, blast radius, and an overall severity rating.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Type name or ID to analyze"
                    }
                },
                "required": ["symbol"]
            }),
        },
        ToolDefinition {
            name: "detect_smells".to_string(),
            description: "Scan for code smells across the repo or within a specific module, file, or symbol scope: god types (>15 properties), excessive dependencies (>10), wide invalidation surfaces (>5 sources), massive inits (>8 params), deep nesting (>5 levels), high fan-out/fan-in (>15 calls), and many extensions (>5). Returns smells sorted by severity.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "module": {
                        "type": "string",
                        "description": "Limit smell analysis to a specific module"
                    },
                    "file": {
                        "type": "string",
                        "description": "Limit smell analysis to symbols declared in a matching file"
                    },
                    "symbol": {
                        "type": "string",
                        "description": "Limit smell analysis to a specific symbol and its local neighborhood"
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters for smells",
                        "default": false
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "default": 200
                    }
                }
            }),
        },
        ToolDefinition {
            name: "get_module_summary".to_string(),
            description: "Get high-level metrics for each module: symbol count, file count, symbols by kind, edge count, cross-module coupling ratio, entry points, and terminals. Sorted by symbol count descending.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "search_concepts".to_string(),
            description: "Resolve a business concept or product term to likely code scopes using stored concept bindings first, then localization, asset, and symbol heuristics.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Business concept text"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of scopes to return (default: 20)",
                        "default": 20
                    },
                    "cluster": {
                        "type": "boolean",
                        "description": "Return score-band clusters for concept scopes instead of the default result JSON",
                        "default": false
                    },
                    "cluster_id": {
                        "type": "string",
                        "description": "Score-band cluster to page: excellent, strong, possible, weak, or unknown"
                    },
                    "cluster_page": {
                        "type": "integer",
                        "description": "1-based page within the selected cluster",
                        "default": 1
                    },
                    "cluster_per_page": {
                        "type": "integer",
                        "description": "Items returned in the selected cluster page",
                        "default": 20
                    },
                    "cluster_candidate_limit": {
                        "type": "integer",
                        "description": "Candidates fetched before score-band clustering",
                        "default": 200
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "get_concept".to_string(),
            description: "Show a stored concept mapping, including aliases and bound symbols.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "term": {
                        "type": "string",
                        "description": "Concept text or alias"
                    }
                },
                "required": ["term"]
            }),
        },
        ToolDefinition {
            name: "bind_concept".to_string(),
            description: "Persist a confirmed concept-to-symbol mapping for future lookups.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "concept": {
                        "type": "string",
                        "description": "Canonical business concept text"
                    },
                    "symbols": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "One or more symbol queries or IDs to bind"
                    }
                },
                "required": ["concept", "symbols"]
            }),
        },
        ToolDefinition {
            name: "add_concept_alias".to_string(),
            description: "Add one or more aliases for a concept in the project concept store.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "concept": {
                        "type": "string",
                        "description": "Canonical business concept text"
                    },
                    "aliases": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Aliases to add"
                    }
                },
                "required": ["concept", "aliases"]
            }),
        },
        ToolDefinition {
            name: "remove_concept".to_string(),
            description: "Remove a concept from the project concept store.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "concept": {
                        "type": "string",
                        "description": "Concept text or alias"
                    }
                },
                "required": ["concept"]
            }),
        },
        ToolDefinition {
            name: "reload".to_string(),
            description: "Reload the graph and search index from disk. Use after running `grapha index` from the CLI to pick up changes without restarting the MCP server.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
    ]
}

fn text_content(text: String) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": text
        }]
    })
}

fn tool_error(message: String) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": message
        }],
        "isError": true
    })
}

fn format_query_error(err: &query::QueryResolveError) -> String {
    match err {
        query::QueryResolveError::NotFound { query } => {
            format!("symbol not found: {query}")
        }
        query::QueryResolveError::Ambiguous { query, candidates } => {
            let mut msg = format!("ambiguous query: {query}\n");
            for c in candidates {
                msg.push_str(&format!(
                    "  - {} [{:?}] in {} ({})\n",
                    c.name,
                    c.kind,
                    c.file,
                    c.locator.as_deref().unwrap_or(c.id.as_str())
                ));
            }
            msg.push_str(&format!("hint: {}", query::ambiguity_hint()));
            msg
        }
        query::QueryResolveError::NotFunction { hint } => hint.clone(),
    }
}

fn serialize_result<T: serde::Serialize>(result: &T) -> Value {
    match serde_json::to_string_pretty(result) {
        Ok(json) => text_content(json),
        Err(e) => tool_error(format!("failed to serialize result: {e}")),
    }
}

fn serialize_result_with_fields<T: serde::Serialize>(
    result: &T,
    fields: Option<FieldSet>,
) -> Value {
    let Some(fields) = fields else {
        return serialize_result(result);
    };

    match serde_json::to_value(result) {
        Ok(mut value) => {
            project_symbol_fields(&mut value, fields);
            match serde_json::to_string_pretty(&value) {
                Ok(json) => text_content(json),
                Err(e) => tool_error(format!("failed to serialize result: {e}")),
            }
        }
        Err(e) => tool_error(format!("failed to serialize result: {e}")),
    }
}

fn project_symbol_fields(value: &mut Value, fields: FieldSet) {
    match value {
        Value::Object(object) => {
            for value in object.values_mut() {
                project_symbol_fields(value, fields);
            }

            if is_symbol_object(object) {
                if !fields.id {
                    object.remove("id");
                }
                if !fields.file {
                    object.remove("file");
                }
                if !fields.locator {
                    object.remove("locator");
                }
                if !fields.module {
                    object.remove("module");
                }
                if !fields.repo {
                    object.remove("repo");
                }
                if !fields.span {
                    object.remove("span");
                }
                if !fields.snippet {
                    object.remove("snippet");
                }
                if !fields.visibility {
                    object.remove("visibility");
                }
                if !fields.signature {
                    object.remove("signature");
                }
                if !fields.doc_comment {
                    object.remove("doc_comment");
                }
                if !fields.annotation {
                    object.remove("annotation");
                }
                if !fields.role {
                    object.remove("role");
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                project_symbol_fields(item, fields);
            }
        }
        _ => {}
    }
}

fn is_symbol_object(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("name")
        && object.contains_key("kind")
        && (object.contains_key("id")
            || object.contains_key("file")
            || object.contains_key("locator"))
}

fn cluster_options_from_arguments(arguments: &Value) -> Option<cluster::ClusterOptions> {
    arguments
        .get("cluster")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        .then(|| {
            cluster::ClusterOptions::new(
                arguments
                    .get("cluster_id")
                    .and_then(|value| value.as_str())
                    .map(String::from),
                arguments
                    .get("cluster_page")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(1) as usize,
                arguments
                    .get("cluster_per_page")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(20) as usize,
                arguments
                    .get("cluster_candidate_limit")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(200)
                    .max(1) as usize,
            )
        })
}

fn limit_from_arguments(arguments: &Value, default: usize) -> usize {
    arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|value| value.max(1) as usize)
        .unwrap_or(default)
}

fn fields_from_arguments(arguments: &Value) -> Option<FieldSet> {
    arguments
        .get("fields")
        .and_then(|v| v.as_str())
        .map(FieldSet::parse)
}

pub fn handle_tool_call(state: &mut McpState, tool_name: &str, arguments: &Value) -> Value {
    match tool_name {
        "search_symbols" => handle_search_symbols(state, arguments),
        "get_index_status" => handle_get_index_status(state),
        "get_symbol_context" => handle_get_symbol_context(state, arguments),
        "annotate_symbol" => handle_annotate_symbol(state, arguments),
        "get_impact" => handle_get_impact(state, arguments),
        "get_file_map" => handle_get_file_map(state, arguments),
        "trace" => handle_trace(state, arguments),
        "get_file_symbols" => handle_get_file_symbols(state, arguments),
        "batch_context" => handle_batch_context(state, arguments),
        "analyze_complexity" => handle_analyze_complexity(state, arguments),
        "detect_smells" => handle_detect_smells(state, arguments),
        "get_module_summary" => handle_get_module_summary(state),
        "search_concepts" => handle_search_concepts(state, arguments),
        "get_concept" => handle_get_concept(state, arguments),
        "bind_concept" => handle_bind_concept(state, arguments),
        "add_concept_alias" => handle_add_concept_alias(state, arguments),
        "remove_concept" => handle_remove_concept(state, arguments),
        "reload" => handle_reload(state),
        _ => tool_error(format!("unknown tool: {tool_name}")),
    }
}

/// Pre-resolve a symbol query using recall to break ties, returning the node ID.
fn resolve_symbol(state: &mut McpState, query: &str) -> Result<String, Value> {
    match recall::resolve_with_recall(&state.graph, query, &mut state.recall) {
        Ok(node) => Ok(node.id.clone()),
        Err(e) => Err(tool_error(format_query_error(&e))),
    }
}

// --- Existing handlers ---

fn handle_search_symbols(state: &McpState, arguments: &Value) -> Value {
    let query_str = match arguments.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return tool_error("missing required parameter: query".to_string()),
    };
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;
    let mut cluster_options = cluster_options_from_arguments(arguments);
    if let Some(options) = cluster_options.as_mut() {
        options.candidate_limit = options.candidate_limit.max(limit);
    }
    let options = search::SearchOptions {
        kind: arguments
            .get("kind")
            .and_then(|v| v.as_str())
            .map(String::from),
        module: arguments
            .get("module")
            .and_then(|v| v.as_str())
            .map(String::from),
        repo: arguments
            .get("repo")
            .and_then(|v| v.as_str())
            .map(String::from),
        file_glob: arguments
            .get("file")
            .and_then(|v| v.as_str())
            .map(String::from),
        role: arguments
            .get("role")
            .and_then(|v| v.as_str())
            .map(String::from),
        fuzzy: arguments
            .get("fuzzy")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        exact_name: arguments
            .get("exact_name")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        declarations_only: arguments
            .get("declarations_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        public_only: arguments
            .get("public_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    let fields = arguments
        .get("fields")
        .and_then(|v| v.as_str())
        .map(FieldSet::parse);

    let candidate_limit = cluster_options
        .as_ref()
        .map(|options| options.candidate_limit)
        .unwrap_or(limit);

    match search::search_filtered(&state.search_index, query_str, candidate_limit, &options) {
        Ok(results) => {
            if let Some(fields) = fields {
                let graph =
                    search::needs_graph_for_projection(fields, false).then_some(&state.graph);
                let annotations = if fields.annotation {
                    annotations::AnnotationStore::for_project_root(&state.project_root)
                        .load_index()
                        .ok()
                } else {
                    None
                };
                let projected =
                    search::project_results(&results, graph, fields, false, annotations.as_ref());
                if let Some(cluster_options) = cluster_options.as_ref() {
                    let raw_scores = results
                        .iter()
                        .map(|result| result.score)
                        .collect::<Vec<_>>();
                    serialize_result(&cluster::search_items(
                        json!({
                            "query": query_str,
                            "total": results.len(),
                        }),
                        &projected,
                        &raw_scores,
                        cluster_options,
                    ))
                } else {
                    serialize_result(&projected)
                }
            } else {
                if let Some(cluster_options) = cluster_options.as_ref() {
                    let raw_scores = results
                        .iter()
                        .map(|result| result.score)
                        .collect::<Vec<_>>();
                    serialize_result(&cluster::search_items(
                        json!({
                            "query": query_str,
                            "total": results.len(),
                        }),
                        &results,
                        &raw_scores,
                        cluster_options,
                    ))
                } else {
                    serialize_result(&results)
                }
            }
        }
        Err(e) => tool_error(format!("search failed: {e}")),
    }
}

fn handle_get_index_status(state: &McpState) -> Value {
    match crate::index_status::load_index_status(&state.project_root, &state.store_path) {
        Ok(status) => serialize_result(&status),
        Err(error) => tool_error(format!("failed to load index status: {error}")),
    }
}

fn handle_get_symbol_context(state: &mut McpState, arguments: &Value) -> Value {
    let query = match arguments.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: symbol".to_string()),
    };
    let symbol_id = match resolve_symbol(state, query) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let cluster_options = cluster_options_from_arguments(arguments);
    let fields = fields_from_arguments(arguments);
    let limit = cluster_options
        .as_ref()
        .map(|options| options.candidate_limit)
        .unwrap_or_else(|| limit_from_arguments(arguments, 20));

    let result = query::context::query_context_with_options(
        &state.graph,
        &symbol_id,
        &query::context::ContextQueryOptions { limit },
    );

    match result {
        Ok(mut result) => {
            if let Ok(annotations) =
                annotations::AnnotationStore::for_project_root(&state.project_root).load_index()
            {
                result.apply_annotations(&state.graph, &annotations);
            }
            if let Some(options) = cluster_options.as_ref() {
                serialize_result(&cluster::context_result(&result, &state.graph, options))
            } else {
                serialize_result_with_fields(&result, fields)
            }
        }
        Err(e) => tool_error(format_query_error(&e)),
    }
}

fn handle_annotate_symbol(state: &mut McpState, arguments: &Value) -> Value {
    let query = match arguments.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: symbol".to_string()),
    };
    let annotation = match arguments
        .get("annotation")
        .or_else(|| arguments.get("text"))
        .and_then(|v| v.as_str())
    {
        Some(s) => s,
        None => return tool_error("missing required parameter: annotation".to_string()),
    };
    let created_by = arguments.get("created_by").and_then(|v| v.as_str());
    let symbol_id = match resolve_symbol(state, query) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let Some(node) = state.graph.nodes.iter().find(|node| node.id == symbol_id) else {
        return tool_error(format!("symbol not found: {query}"));
    };

    match annotations::AnnotationStore::for_project_root(&state.project_root)
        .upsert_for_node(node, annotation, created_by)
    {
        Ok(result) => serialize_result(&result),
        Err(error) => tool_error(format!("failed to save annotation: {error}")),
    }
}

fn handle_get_impact(state: &mut McpState, arguments: &Value) -> Value {
    let query = match arguments.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: symbol".to_string()),
    };
    let symbol_id = match resolve_symbol(state, query) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let depth = arguments.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
    let cluster_options = cluster_options_from_arguments(arguments);
    let fields = fields_from_arguments(arguments);
    let limit = cluster_options
        .as_ref()
        .map(|options| options.candidate_limit)
        .unwrap_or_else(|| limit_from_arguments(arguments, 20));

    let result = query::impact::query_impact_with_options(
        &state.graph,
        &symbol_id,
        depth,
        &query::impact::ImpactQueryOptions { limit },
    );

    match result {
        Ok(result) => {
            if let Some(options) = cluster_options.as_ref() {
                serialize_result(&cluster::impact_result(&result, options))
            } else {
                serialize_result_with_fields(&result, fields)
            }
        }
        Err(e) => tool_error(format_query_error(&e)),
    }
}

fn handle_get_file_map(state: &McpState, arguments: &Value) -> Value {
    let module = arguments.get("module").and_then(|v| v.as_str());
    let result = query::map::file_map(&state.graph, module);
    serialize_result(&result)
}

fn handle_trace(state: &mut McpState, arguments: &Value) -> Value {
    let query = match arguments.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: symbol".to_string()),
    };
    let symbol_id = match resolve_symbol(state, query) {
        Ok(id) => id,
        Err(e) => return e,
    };
    let direction = arguments
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("forward");
    let depth = arguments.get("depth").and_then(|v| v.as_u64());
    let cluster_options = cluster_options_from_arguments(arguments);
    let fields = fields_from_arguments(arguments);
    let limit = cluster_options
        .as_ref()
        .map(|options| options.candidate_limit)
        .unwrap_or_else(|| limit_from_arguments(arguments, 20));

    match direction {
        "forward" => {
            let max_depth = depth.unwrap_or(10) as usize;
            let result = query::trace::query_trace_with_options(
                &state.graph,
                &symbol_id,
                max_depth,
                &query::trace::TraceQueryOptions { limit },
            );
            match result {
                Ok(result) => {
                    if let Some(options) = cluster_options.as_ref() {
                        serialize_result(&cluster::trace_result(&result, options))
                    } else {
                        serialize_result_with_fields(&result, fields)
                    }
                }
                Err(e) => tool_error(format_query_error(&e)),
            }
        }
        "reverse" => {
            let max_depth = depth.map(|d| d as usize);
            let result = query::reverse::query_reverse_with_options(
                &state.graph,
                &symbol_id,
                max_depth,
                &query::reverse::ReverseQueryOptions { limit },
            );
            match result {
                Ok(result) => {
                    if let Some(options) = cluster_options.as_ref() {
                        serialize_result(&cluster::reverse_result(&result, options))
                    } else {
                        serialize_result_with_fields(&result, fields)
                    }
                }
                Err(e) => tool_error(format_query_error(&e)),
            }
        }
        other => tool_error(format!(
            "invalid direction: {other} (expected \"forward\" or \"reverse\")"
        )),
    }
}

// --- New handlers ---

fn handle_get_file_symbols(state: &McpState, arguments: &Value) -> Value {
    let file = match arguments.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return tool_error("missing required parameter: file".to_string()),
    };

    let result = query::file_symbols::query_file_symbols(&state.graph, file);
    if result.total == 0 {
        return tool_error(format!("no symbols found in file matching: {file}"));
    }
    if let Some(options) = cluster_options_from_arguments(arguments).as_ref() {
        serialize_result(&cluster::file_symbols_result(&result, options))
    } else {
        serialize_result(&result)
    }
}

fn handle_batch_context(state: &mut McpState, arguments: &Value) -> Value {
    let symbols = match arguments.get("symbols").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return tool_error("missing required parameter: symbols (array)".to_string()),
    };

    let symbol_strs: Vec<&str> = symbols.iter().filter_map(|v| v.as_str()).collect();

    if symbol_strs.is_empty() {
        return tool_error("symbols array is empty".to_string());
    }

    if symbol_strs.len() > 20 {
        return tool_error("batch_context supports at most 20 symbols per call".to_string());
    }

    let annotations = annotations::AnnotationStore::for_project_root(&state.project_root)
        .load_index()
        .ok();
    let fields = fields_from_arguments(arguments);
    let limit = limit_from_arguments(arguments, 20);
    let mut results: Vec<Value> = Vec::with_capacity(symbol_strs.len());
    for symbol in &symbol_strs {
        let resolved = resolve_symbol(state, symbol);
        let query_id = match &resolved {
            Ok(id) => id.as_str(),
            Err(_) => symbol,
        };
        match query::context::query_context_with_options(
            &state.graph,
            query_id,
            &query::context::ContextQueryOptions { limit },
        ) {
            Ok(mut ctx) => {
                if let Some(annotations) = annotations.as_ref() {
                    ctx.apply_annotations(&state.graph, annotations);
                }
                let mut result = serde_json::to_value(&ctx).unwrap_or(Value::Null);
                if let Some(fields) = fields {
                    project_symbol_fields(&mut result, fields);
                }
                results.push(json!({
                    "query": symbol,
                    "result": result,
                }));
            }
            Err(e) => {
                results.push(json!({
                    "query": symbol,
                    "error": format_query_error(&e),
                }));
            }
        }
    }

    serialize_result(&results)
}

fn handle_analyze_complexity(state: &mut McpState, arguments: &Value) -> Value {
    let query = match arguments.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: symbol".to_string()),
    };
    let symbol_id = match resolve_symbol(state, query) {
        Ok(id) => id,
        Err(e) => return e,
    };

    match query::complexity::query_complexity(&state.graph, &symbol_id) {
        Ok(result) => serialize_result(&result),
        Err(e) => tool_error(format_query_error(&e)),
    }
}

fn handle_detect_smells(state: &McpState, arguments: &Value) -> Value {
    let module_filter = arguments.get("module").and_then(|v| v.as_str());
    let file_filter = arguments.get("file").and_then(|v| v.as_str());
    let symbol_filter = arguments.get("symbol").and_then(|v| v.as_str());

    let scope_count = [module_filter, file_filter, symbol_filter]
        .into_iter()
        .flatten()
        .count();
    if scope_count > 1 {
        return tool_error("choose only one of module, file, or symbol".to_string());
    }

    let result = if let Some(file) = file_filter {
        query::smells::detect_smells_for_file(&state.graph, file)
    } else if let Some(symbol_query) = symbol_filter {
        let node = match query::resolve_node(&state.graph, symbol_query) {
            Ok(node) => node,
            Err(e) => return tool_error(format_query_error(&e)),
        };
        query::smells::detect_smells_for_symbol(&state.graph, &node.id)
    } else if let Some(module) = module_filter {
        query::smells::detect_smells_for_module(&state.graph, module)
    } else {
        query::smells::detect_smells(&state.graph)
    };

    if let Some(options) = cluster_options_from_arguments(arguments).as_ref() {
        serialize_result(&cluster::smells_result(&result, options))
    } else {
        serialize_result(&result)
    }
}

fn handle_get_module_summary(state: &McpState) -> Value {
    let result = query::module_summary::query_module_summary(&state.graph);
    serialize_result(&result)
}

fn handle_search_concepts(state: &McpState, arguments: &Value) -> Value {
    let query = match arguments.get("query").and_then(|v| v.as_str()) {
        Some(query) => query,
        None => return tool_error("missing required parameter: query".to_string()),
    };
    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(concepts::DEFAULT_CONCEPT_SEARCH_LIMIT as u64) as usize;
    let mut cluster_options = cluster_options_from_arguments(arguments);
    if let Some(options) = cluster_options.as_mut() {
        options.candidate_limit = options.candidate_limit.max(limit);
    }

    let concept_index = match concepts::load_concept_index_from_store(&state.store_path) {
        Ok(index) => index,
        Err(error) => return tool_error(format!("failed to load concept store: {error}")),
    };
    let catalogs =
        localization::load_catalog_index_from_store(&state.store_path).unwrap_or_default();
    let assets_index = assets::load_asset_index_from_store(&state.store_path).unwrap_or_default();
    let annotations = annotations::AnnotationStore::for_project_root(&state.project_root)
        .load_index()
        .ok();

    match concepts::search_concepts_with_annotations(
        &state.graph,
        &state.search_index,
        &concept_index,
        &catalogs,
        &assets_index,
        query,
        cluster_options
            .as_ref()
            .map(|options| options.candidate_limit)
            .unwrap_or(limit),
        annotations.as_ref(),
    ) {
        Ok(result) => {
            if let Some(options) = cluster_options.as_ref() {
                serialize_result(&cluster::concept_search_full_result(&result, options))
            } else {
                serialize_result(&result)
            }
        }
        Err(error) => tool_error(format!("concept search failed: {error}")),
    }
}

fn handle_get_concept(state: &McpState, arguments: &Value) -> Value {
    let term = match arguments.get("term").and_then(|v| v.as_str()) {
        Some(term) => term,
        None => return tool_error("missing required parameter: term".to_string()),
    };

    let concept_index = match concepts::load_concept_index_from_store(&state.store_path) {
        Ok(index) => index,
        Err(error) => return tool_error(format!("failed to load concept store: {error}")),
    };

    match concepts::show_concept(&state.graph, &concept_index, term) {
        Ok(result) => serialize_result(&result),
        Err(error) => tool_error(error.to_string()),
    }
}

fn handle_bind_concept(state: &mut McpState, arguments: &Value) -> Value {
    let concept = match arguments.get("concept").and_then(|v| v.as_str()) {
        Some(concept) => concept,
        None => return tool_error("missing required parameter: concept".to_string()),
    };
    let symbols = match arguments.get("symbols").and_then(|v| v.as_array()) {
        Some(symbols) => symbols,
        None => return tool_error("missing required parameter: symbols".to_string()),
    };
    if symbols.is_empty() {
        return tool_error("symbols array is empty".to_string());
    }

    let mut unique_ids = std::collections::BTreeSet::new();
    for symbol in symbols {
        let Some(symbol_query) = symbol.as_str() else {
            return tool_error("symbols must be an array of strings".to_string());
        };
        let node = match query::resolve_node(&state.graph, symbol_query) {
            Ok(node) => node,
            Err(error) => return tool_error(format_query_error(&error)),
        };
        unique_ids.insert(node.id.clone());
    }

    let mut concept_index = match concepts::load_concept_index_from_store(&state.store_path) {
        Ok(index) => index,
        Err(error) => return tool_error(format!("failed to load concept store: {error}")),
    };
    let result = match concept_index.bind_concept(
        concept,
        &unique_ids.into_iter().collect::<Vec<_>>(),
        vec![concepts::ConceptEvidence {
            kind: "manual".to_string(),
            value: concept.trim().to_string(),
            match_kind: "confirmed".to_string(),
            table: None,
            key: None,
            source_value: None,
            ui_path: Vec::new(),
            note: Some("manual concept binding".to_string()),
        }],
    ) {
        Ok(result) => result,
        Err(error) => return tool_error(error.to_string()),
    };

    match concepts::save_concept_index_to_store(&state.store_path, &concept_index) {
        Ok(()) => serialize_result(&result),
        Err(error) => tool_error(format!("failed to save concept store: {error}")),
    }
}

fn handle_add_concept_alias(state: &McpState, arguments: &Value) -> Value {
    let concept = match arguments.get("concept").and_then(|v| v.as_str()) {
        Some(concept) => concept,
        None => return tool_error("missing required parameter: concept".to_string()),
    };
    let aliases = match arguments.get("aliases").and_then(|v| v.as_array()) {
        Some(aliases) => aliases,
        None => return tool_error("missing required parameter: aliases".to_string()),
    };
    if aliases.is_empty() {
        return tool_error("aliases array is empty".to_string());
    }

    let mut concept_index = match concepts::load_concept_index_from_store(&state.store_path) {
        Ok(index) => index,
        Err(error) => return tool_error(format!("failed to load concept store: {error}")),
    };
    let alias_values: Vec<String> = aliases
        .iter()
        .filter_map(|value| value.as_str().map(ToString::to_string))
        .collect();
    if alias_values.len() != aliases.len() {
        return tool_error("aliases must be an array of strings".to_string());
    }

    let result = match concept_index.add_aliases(concept, &alias_values) {
        Ok(result) => result,
        Err(error) => return tool_error(error.to_string()),
    };

    match concepts::save_concept_index_to_store(&state.store_path, &concept_index) {
        Ok(()) => serialize_result(&result),
        Err(error) => tool_error(format!("failed to save concept store: {error}")),
    }
}

fn handle_remove_concept(state: &McpState, arguments: &Value) -> Value {
    let concept = match arguments.get("concept").and_then(|v| v.as_str()) {
        Some(concept) => concept,
        None => return tool_error("missing required parameter: concept".to_string()),
    };

    let mut concept_index = match concepts::load_concept_index_from_store(&state.store_path) {
        Ok(index) => index,
        Err(error) => return tool_error(format!("failed to load concept store: {error}")),
    };
    let result = concept_index.remove_concept(concept);

    match concepts::save_concept_index_to_store(&state.store_path, &concept_index) {
        Ok(()) => serialize_result(&result),
        Err(error) => tool_error(format!("failed to save concept store: {error}")),
    }
}

fn handle_reload(state: &mut McpState) -> Value {
    let db_path = state.store_path.join("grapha.db");
    let search_index_path = state.store_path.join("search_index");

    // Reload graph from SQLite
    let store = crate::store::sqlite::SqliteStore::new(db_path);
    let graph = match store.load() {
        Ok(g) => g,
        Err(e) => return tool_error(format!("failed to reload graph: {e}")),
    };

    // Reload search index
    let search_index = if search_index_path.exists() {
        match tantivy::Index::open_in_dir(&search_index_path) {
            Ok(idx) => idx,
            Err(e) => return tool_error(format!("failed to reload search index: {e}")),
        }
    } else {
        match search::build_index(&graph, &search_index_path) {
            Ok(idx) => idx,
            Err(e) => return tool_error(format!("failed to build search index: {e}")),
        }
    };

    let node_count = graph.nodes.len();
    let edge_count = graph.edges.len();

    state.graph = graph;
    state.search_index = search_index;

    // Prune recall entries that reference nodes no longer in the graph
    let valid_ids: std::collections::HashSet<&str> =
        state.graph.nodes.iter().map(|n| n.id.as_str()).collect();
    state.recall.prune(&valid_ids);

    text_content(format!(
        "Reloaded successfully: {node_count} nodes, {edge_count} edges"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, EdgeKind, Node, NodeKind, Span, Visibility};
    use std::collections::HashMap;

    #[test]
    fn tool_definitions_count() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 18);

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"search_symbols"));
        assert!(names.contains(&"get_index_status"));
        assert!(names.contains(&"get_symbol_context"));
        assert!(names.contains(&"annotate_symbol"));
        assert!(names.contains(&"get_impact"));
        assert!(names.contains(&"get_file_map"));
        assert!(names.contains(&"trace"));
        assert!(names.contains(&"get_file_symbols"));
        assert!(names.contains(&"batch_context"));
        assert!(names.contains(&"analyze_complexity"));
        assert!(names.contains(&"detect_smells"));
        assert!(names.contains(&"get_module_summary"));
        assert!(names.contains(&"search_concepts"));
        assert!(names.contains(&"get_concept"));
        assert!(names.contains(&"bind_concept"));
        assert!(names.contains(&"add_concept_alias"));
        assert!(names.contains(&"remove_concept"));
        assert!(names.contains(&"reload"));
    }

    #[test]
    fn unknown_tool_returns_error() {
        let mut state = make_test_state();
        let result = handle_tool_call(&mut state, "nonexistent", &json!({}));
        assert!(
            result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    #[test]
    fn search_symbols_missing_query_returns_error() {
        let mut state = make_test_state();
        let result = handle_tool_call(&mut state, "search_symbols", &json!({}));
        assert!(
            result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    #[test]
    fn get_file_symbols_missing_file_returns_error() {
        let mut state = make_test_state();
        let result = handle_tool_call(&mut state, "get_file_symbols", &json!({}));
        assert!(
            result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    #[test]
    fn batch_context_empty_array_returns_error() {
        let mut state = make_test_state();
        let result = handle_tool_call(&mut state, "batch_context", &json!({"symbols": []}));
        assert!(
            result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    #[test]
    fn get_symbol_context_respects_limit_and_fields() {
        let mut state = make_test_state_with_context();
        let result = handle_tool_call(
            &mut state,
            "get_symbol_context",
            &json!({
                "symbol": "target",
                "limit": 1,
                "fields": "locator"
            }),
        );

        assert!(
            !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
        let parsed = text_json(&result);
        assert_eq!(parsed["total_callers"], 2);
        assert_eq!(parsed["callers"].as_array().unwrap().len(), 1);
        let caller = &parsed["callers"][0];
        assert_eq!(caller["name"], "caller_a");
        assert!(caller.get("locator").is_some());
        assert!(caller.get("id").is_none());
        assert!(caller.get("file").is_none());
        assert!(caller.get("span").is_none());
    }

    #[test]
    fn batch_context_respects_limit_and_fields() {
        let mut state = make_test_state_with_context();
        let result = handle_tool_call(
            &mut state,
            "batch_context",
            &json!({
                "symbols": ["target"],
                "limit": 1,
                "fields": "file"
            }),
        );

        assert!(
            !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
        let parsed = text_json(&result);
        let entry = &parsed[0]["result"];
        assert_eq!(entry["total_callers"], 2);
        assert_eq!(entry["callers"].as_array().unwrap().len(), 1);
        let caller = &entry["callers"][0];
        assert_eq!(caller["file"], "src/caller_a.rs");
        assert!(caller.get("locator").is_none());
        assert!(caller.get("id").is_none());
    }

    #[test]
    fn detect_smells_on_empty_graph() {
        let mut state = make_test_state();
        let result = handle_tool_call(&mut state, "detect_smells", &json!({}));
        assert!(
            !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    #[test]
    fn detect_smells_rejects_multiple_scopes() {
        let mut state = make_test_state();
        let result = handle_tool_call(
            &mut state,
            "detect_smells",
            &json!({"module": "Room", "file": "RoomPage.swift"}),
        );
        assert!(
            result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    #[test]
    fn detect_smells_symbol_scope_limits_results() {
        let mut state = make_test_state_with_smells();
        let result = handle_tool_call(&mut state, "detect_smells", &json!({"symbol": "MainView"}));
        assert!(
            !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );

        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["smells"][0]["symbol"]["name"], "MainView");
    }

    #[test]
    fn get_module_summary_on_empty_graph() {
        let mut state = make_test_state();
        let result = handle_tool_call(&mut state, "get_module_summary", &json!({}));
        assert!(
            !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );
    }

    fn text_json(result: &Value) -> Value {
        let text = result["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    fn make_test_state() -> McpState {
        let graph = Graph {
            version: String::new(),
            nodes: vec![],
            edges: vec![],
        };
        let schema = tantivy::schema::Schema::builder().build();
        let index = Index::create_in_ram(schema);
        McpState {
            graph,
            search_index: index,
            project_root: PathBuf::from("/tmp/project"),
            store_path: PathBuf::from("/tmp/test"),
            recall: Recall::new(),
        }
    }

    fn make_test_state_with_context() -> McpState {
        fn node(id: &str, name: &str, file: &str) -> Node {
            Node {
                id: id.into(),
                kind: NodeKind::Function,
                name: name.into(),
                file: PathBuf::from(file),
                span: Span {
                    start: [1, 0],
                    end: [3, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: Some(format!("fn {name}()")),
                doc_comment: None,
                module: Some("App".to_string()),
                snippet: Some(format!("fn {name}() {{}}")),
                repo: None,
            }
        }

        fn edge(source: &str, target: &str) -> Edge {
            Edge {
                source: source.into(),
                target: target.into(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![],
                repo: None,
            }
        }

        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node("src/caller_a.rs::caller_a", "caller_a", "src/caller_a.rs"),
                node("src/caller_b.rs::caller_b", "caller_b", "src/caller_b.rs"),
                node("src/target.rs::target", "target", "src/target.rs"),
            ],
            edges: vec![
                edge("src/caller_a.rs::caller_a", "src/target.rs::target"),
                edge("src/caller_b.rs::caller_b", "src/target.rs::target"),
            ],
        };
        let schema = tantivy::schema::Schema::builder().build();
        let index = Index::create_in_ram(schema);
        McpState {
            graph,
            search_index: index,
            project_root: PathBuf::from("/tmp/project"),
            store_path: PathBuf::from("/tmp/test"),
            recall: Recall::new(),
        }
    }

    fn make_test_state_with_smells() -> McpState {
        fn node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
            Node {
                id: id.into(),
                kind,
                name: name.into(),
                file: PathBuf::from(file),
                span: Span {
                    start: [1, 0],
                    end: [10, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: Some("App".to_string()),
                snippet: None,
                repo: None,
            }
        }

        fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
            Edge {
                source: source.into(),
                target: target.into(),
                kind,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![],
                repo: None,
            }
        }

        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node(
                    "src/main.rs::MainView",
                    "MainView",
                    NodeKind::Struct,
                    "src/main.rs",
                ),
                node(
                    "src/main.rs::MainView::L1",
                    "L1",
                    NodeKind::View,
                    "src/main.rs",
                ),
                node(
                    "src/main.rs::MainView::L2",
                    "L2",
                    NodeKind::View,
                    "src/main.rs",
                ),
                node(
                    "src/main.rs::MainView::L3",
                    "L3",
                    NodeKind::View,
                    "src/main.rs",
                ),
                node(
                    "src/main.rs::MainView::L4",
                    "L4",
                    NodeKind::View,
                    "src/main.rs",
                ),
                node(
                    "src/main.rs::MainView::L5",
                    "L5",
                    NodeKind::View,
                    "src/main.rs",
                ),
                node(
                    "src/main.rs::MainView::L6",
                    "L6",
                    NodeKind::View,
                    "src/main.rs",
                ),
                node(
                    "src/other.rs::SecondaryView",
                    "SecondaryView",
                    NodeKind::Struct,
                    "src/other.rs",
                ),
                node(
                    "src/other.rs::SecondaryView::L1",
                    "L1",
                    NodeKind::View,
                    "src/other.rs",
                ),
                node(
                    "src/other.rs::SecondaryView::L2",
                    "L2",
                    NodeKind::View,
                    "src/other.rs",
                ),
                node(
                    "src/other.rs::SecondaryView::L3",
                    "L3",
                    NodeKind::View,
                    "src/other.rs",
                ),
                node(
                    "src/other.rs::SecondaryView::L4",
                    "L4",
                    NodeKind::View,
                    "src/other.rs",
                ),
                node(
                    "src/other.rs::SecondaryView::L5",
                    "L5",
                    NodeKind::View,
                    "src/other.rs",
                ),
                node(
                    "src/other.rs::SecondaryView::L6",
                    "L6",
                    NodeKind::View,
                    "src/other.rs",
                ),
            ],
            edges: vec![
                edge(
                    "src/main.rs::MainView",
                    "src/main.rs::MainView::L1",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/main.rs::MainView::L1",
                    "src/main.rs::MainView::L2",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/main.rs::MainView::L2",
                    "src/main.rs::MainView::L3",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/main.rs::MainView::L3",
                    "src/main.rs::MainView::L4",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/main.rs::MainView::L4",
                    "src/main.rs::MainView::L5",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/main.rs::MainView::L5",
                    "src/main.rs::MainView::L6",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/other.rs::SecondaryView",
                    "src/other.rs::SecondaryView::L1",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/other.rs::SecondaryView::L1",
                    "src/other.rs::SecondaryView::L2",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/other.rs::SecondaryView::L2",
                    "src/other.rs::SecondaryView::L3",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/other.rs::SecondaryView::L3",
                    "src/other.rs::SecondaryView::L4",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/other.rs::SecondaryView::L4",
                    "src/other.rs::SecondaryView::L5",
                    EdgeKind::Contains,
                ),
                edge(
                    "src/other.rs::SecondaryView::L5",
                    "src/other.rs::SecondaryView::L6",
                    EdgeKind::Contains,
                ),
            ],
        };
        let schema = tantivy::schema::Schema::builder().build();
        let index = Index::create_in_ram(schema);
        McpState {
            graph,
            search_index: index,
            project_root: PathBuf::from("/tmp/project"),
            store_path: PathBuf::from("/tmp/test"),
            recall: Recall::new(),
        }
    }
}
