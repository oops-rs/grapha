use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::fields::FieldSet;
use crate::query;

use super::{AnnotationServiceState, AppState};

pub async fn get_graph(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::to_value(&state.graph).unwrap_or_default())
}

pub async fn get_entries(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let result = query::entries::query_entries(&state.graph);
    Json(serde_json::to_value(&result).unwrap_or_default())
}

#[derive(Serialize)]
struct QueryErrorPayload {
    error: &'static str,
    query: String,
    candidates: Vec<query::QueryCandidate>,
    hint: &'static str,
}

fn query_response<T: Serialize>(result: Result<T, query::QueryResolveError>) -> Response {
    match result {
        Ok(value) => Json(serde_json::to_value(&value).unwrap_or_default()).into_response(),
        Err(query::QueryResolveError::NotFound { .. }) => StatusCode::NOT_FOUND.into_response(),
        Err(query::QueryResolveError::Ambiguous { query, candidates }) => (
            StatusCode::BAD_REQUEST,
            Json(QueryErrorPayload {
                error: "ambiguous",
                query,
                candidates,
                hint: query::ambiguity_hint(),
            }),
        )
            .into_response(),
        Err(query::QueryResolveError::NotFunction { hint }) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": hint })),
        )
            .into_response(),
    }
}

pub async fn get_context(
    State(state): State<Arc<AppState>>,
    Path(symbol): Path<String>,
) -> impl IntoResponse {
    let decoded = urlencoding::decode(&symbol).unwrap_or_default();
    query_response(query::context::query_context(&state.graph, &decoded))
}

pub async fn get_trace(
    State(state): State<Arc<AppState>>,
    Path(symbol): Path<String>,
) -> impl IntoResponse {
    let decoded = urlencoding::decode(&symbol).unwrap_or_default();
    query_response(query::trace::query_trace(&state.graph, &decoded, 10))
}

pub async fn get_reverse(
    State(state): State<Arc<AppState>>,
    Path(symbol): Path<String>,
) -> impl IntoResponse {
    let decoded = urlencoding::decode(&symbol).unwrap_or_default();
    query_response(query::reverse::query_reverse(&state.graph, &decoded, None))
}

pub async fn get_index_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let status = match crate::index_status::load_index_status(
        &state.project_path,
        &state.project_path.join(".grapha"),
    ) {
        Ok(status) => serde_json::to_value(status).unwrap_or_default(),
        Err(error) => serde_json::json!({
            "error": error.to_string()
        }),
    };
    Json(status)
}

#[derive(Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub kind: Option<String>,
    pub module: Option<String>,
    pub repo: Option<String>,
    pub file: Option<String>,
    pub role: Option<String>,
    #[serde(default)]
    pub fuzzy: bool,
    #[serde(default)]
    pub exact_name: bool,
    #[serde(default)]
    pub declarations_only: bool,
    #[serde(default)]
    pub public_only: bool,
    #[serde(default)]
    pub context: bool,
    pub fields: Option<String>,
}

fn default_limit() -> usize {
    20
}

pub async fn get_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> Json<serde_json::Value> {
    let options = crate::search::SearchOptions {
        kind: params.kind,
        module: params.module,
        repo: params.repo,
        file_glob: params.file,
        role: params.role,
        fuzzy: params.fuzzy,
        exact_name: params.exact_name,
        declarations_only: params.declarations_only,
        public_only: params.public_only,
    };
    let results =
        crate::search::search_filtered(&state.search_index, &params.q, params.limit, &options)
            .unwrap_or_default();
    let fields = params
        .fields
        .as_deref()
        .map(FieldSet::parse)
        .unwrap_or_default();
    let graph =
        crate::search::needs_graph_for_projection(fields, params.context).then_some(&state.graph);
    let annotations = if fields.annotation {
        crate::annotations::AnnotationStore::for_project_root(&state.project_path)
            .load_index()
            .ok()
    } else {
        None
    };
    let projected = crate::search::project_results(
        &results,
        graph,
        fields,
        params.context,
        annotations.as_ref(),
    );
    let index_status = crate::index_status::load_index_status(
        &state.project_path,
        &state.project_path.join(".grapha"),
    )
    .ok();
    Json(serde_json::json!({
        "results": projected,
        "total": results.len(),
        "index_status": index_status
    }))
}

#[derive(Deserialize)]
pub struct AnnotationUpsertRequest {
    pub symbol: String,
    pub annotation: String,
    pub created_by: Option<String>,
}

#[derive(Deserialize)]
pub struct AnnotationSyncRequest {
    #[serde(default)]
    pub project: Option<crate::data_paths::ProjectIdentity>,
    pub annotations: Vec<crate::annotations::SymbolAnnotationRecord>,
}

#[derive(Deserialize)]
pub struct StandaloneAnnotationListParams {
    pub project_id: Option<String>,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": message.into()
        })),
    )
        .into_response()
}

pub async fn list_annotations(State(state): State<Arc<AppState>>) -> Response {
    let store = crate::annotations::AnnotationStore::for_project_root(&state.project_path);
    match store.list_records() {
        Ok(records) => {
            let total = records.len();
            Json(serde_json::json!({
                "project": crate::data_paths::project_identity(&state.project_path),
                "annotations": records,
                "total": total
            }))
            .into_response()
        }
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load annotations: {error}"),
        ),
    }
}

pub async fn get_annotation(
    State(state): State<Arc<AppState>>,
    Path(symbol): Path<String>,
) -> Response {
    let decoded = urlencoding::decode(&symbol).unwrap_or_default();
    let node = match query::resolve_node(&state.graph, &decoded) {
        Ok(node) => node,
        Err(error) => return query_response::<serde_json::Value>(Err(error)),
    };

    match crate::annotations::AnnotationStore::for_project_root(&state.project_path)
        .get_for_node(node)
    {
        Ok(Some(annotation)) => Json(annotation).into_response(),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            format!("no annotation stored for symbol: {decoded}"),
        ),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load annotation: {error}"),
        ),
    }
}

pub async fn post_annotation(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AnnotationUpsertRequest>,
) -> Response {
    let annotation = payload.annotation.trim();
    if annotation.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "annotation text cannot be empty");
    }

    let node = match query::resolve_node(&state.graph, &payload.symbol) {
        Ok(node) => node,
        Err(error) => return query_response::<serde_json::Value>(Err(error)),
    };

    match crate::annotations::AnnotationStore::for_project_root(&state.project_path)
        .upsert_for_node(node, annotation, payload.created_by.as_deref())
    {
        Ok(annotation) => Json(annotation).into_response(),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to save annotation: {error}"),
        ),
    }
}

pub async fn sync_annotations(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AnnotationSyncRequest>,
) -> Response {
    let store = crate::annotations::AnnotationStore::for_project_root(&state.project_path);
    match store.merge_records(&payload.annotations) {
        Ok(merged) => Json(serde_json::json!({
            "merged": merged,
            "received": payload.annotations.len()
        }))
        .into_response(),
        Err(error) => error_response(
            StatusCode::BAD_REQUEST,
            format!("failed to merge annotations: {error}"),
        ),
    }
}

fn request_project_id(
    project_id: Option<&str>,
    records: &[crate::annotations::SymbolAnnotationRecord],
) -> anyhow::Result<String> {
    if let Some(project_id) = project_id.and_then(non_empty) {
        return Ok(project_id.to_string());
    }
    records
        .iter()
        .find_map(|record| non_empty(&record.project_id))
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("project_id query parameter is required"))
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub async fn list_standalone_annotations(
    State(state): State<Arc<AnnotationServiceState>>,
    Query(params): Query<StandaloneAnnotationListParams>,
) -> Response {
    let project_id = match request_project_id(params.project_id.as_deref(), &[]) {
        Ok(project_id) => project_id,
        Err(error) => {
            state
                .log
                .event(format!("annotations list rejected error=\"{error}\""));
            return error_response(StatusCode::BAD_REQUEST, error.to_string());
        }
    };
    state.log.event(format!(
        "annotations list requested project_id={project_id}"
    ));
    let store = match crate::annotations::AnnotationStore::for_project_id_with_data_root(
        &project_id,
        &state.data_root,
    ) {
        Ok(store) => store,
        Err(error) => {
            state.log.event(format!(
                "annotations list rejected project_id={project_id} error=\"{error}\""
            ));
            return error_response(StatusCode::BAD_REQUEST, error.to_string());
        }
    };

    match store.list_records() {
        Ok(records) => {
            let total = records.len();
            state.log.event(format!(
                "annotations list completed project_id={project_id} total={total}"
            ));
            Json(serde_json::json!({
                "project": {
                    "project_id": project_id,
                },
                "annotations": records,
                "total": total
            }))
            .into_response()
        }
        Err(error) => {
            state.log.event(format!(
                "annotations list failed project_id={project_id} error=\"{error}\""
            ));
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load annotations: {error}"),
            )
        }
    }
}

pub async fn sync_standalone_annotations(
    State(state): State<Arc<AnnotationServiceState>>,
    Json(payload): Json<AnnotationSyncRequest>,
) -> Response {
    let fallback_project_id = payload
        .project
        .as_ref()
        .map(|project| project.project_id.as_str());
    let mut grouped: BTreeMap<String, Vec<crate::annotations::SymbolAnnotationRecord>> =
        BTreeMap::new();

    for mut record in payload.annotations {
        let project_id =
            match request_project_id(fallback_project_id, std::slice::from_ref(&record)) {
                Ok(project_id) => project_id,
                Err(error) => {
                    state
                        .log
                        .event(format!("annotations sync rejected error=\"{error}\""));
                    return error_response(StatusCode::BAD_REQUEST, error.to_string());
                }
            };
        if record.project_id.trim().is_empty() {
            record.project_id = project_id.clone();
        }
        grouped.entry(project_id).or_default().push(record);
    }

    let received = grouped.values().map(Vec::len).sum::<usize>();
    state.log.event(format!(
        "annotations sync requested projects={} received={received}",
        grouped.len()
    ));
    let mut merged = 0usize;
    for (project_id, records) in grouped {
        let store = match crate::annotations::AnnotationStore::for_project_id_with_data_root(
            &project_id,
            &state.data_root,
        ) {
            Ok(store) => store,
            Err(error) => {
                state.log.event(format!(
                    "annotations sync rejected project_id={project_id} error=\"{error}\""
                ));
                return error_response(StatusCode::BAD_REQUEST, error.to_string());
            }
        };
        match store.merge_records(&records) {
            Ok(count) => {
                merged += count;
                state.log.event(format!(
                    "annotations sync merged project_id={project_id} received={} merged={count}",
                    records.len()
                ));
            }
            Err(error) => {
                state.log.event(format!(
                    "annotations sync failed project_id={project_id} error=\"{error}\""
                ));
                return error_response(
                    StatusCode::BAD_REQUEST,
                    format!("failed to merge annotations: {error}"),
                );
            }
        }
    }

    state.log.event(format!(
        "annotations sync completed received={received} merged={merged}"
    ));
    Json(serde_json::json!({
        "merged": merged,
        "received": received
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search;
    use crate::serve::{AnnotationServiceState, AppState};
    use grapha_core::graph::{Edge, EdgeKind, Graph, Node, NodeKind, NodeRole, Span, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_state() -> (Arc<AppState>, tempfile::TempDir) {
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
                    doc_comment: None,
                    module: Some("App".into()),
                    snippet: Some("fn main() { helper(); }".into()),
                    repo: None,
                },
                Node {
                    id: "app::helper".into(),
                    kind: NodeKind::Function,
                    name: "helper".into(),
                    file: "src/lib.rs".into(),
                    span: Span {
                        start: [5, 0],
                        end: [5, 12],
                    },
                    visibility: Visibility::Private,
                    metadata: HashMap::new(),
                    role: None,
                    signature: Some("fn helper()".into()),
                    doc_comment: None,
                    module: Some("Core".into()),
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
        let dir = tempdir().unwrap();
        let index = search::build_index(&graph, dir.path()).unwrap();
        (
            Arc::new(AppState {
                project_path: PathBuf::from("."),
                graph,
                search_index: index,
            }),
            dir,
        )
    }

    fn annotation_record(
        project_id: &str,
        branch: &str,
        symbol_key: &str,
    ) -> crate::annotations::SymbolAnnotationRecord {
        crate::annotations::SymbolAnnotationRecord {
            project_id: project_id.to_string(),
            branch: branch.to_string(),
            repo: String::new(),
            symbol_key: symbol_key.to_string(),
            text: "Explains a reusable invariant.".to_string(),
            created_by: Some("test".to_string()),
            created_at: "1".to_string(),
            updated_at: "2".to_string(),
            symbol_fingerprint: None,
        }
    }

    #[tokio::test]
    async fn search_api_applies_filters_and_context() {
        let (state, _dir) = make_state();
        let response = get_search(
            State(state),
            Query(SearchParams {
                q: "main".into(),
                limit: 10,
                kind: Some("function".into()),
                module: Some("App".into()),
                repo: None,
                file: Some("main.rs".into()),
                role: Some("entry_point".into()),
                fuzzy: false,
                exact_name: false,
                declarations_only: false,
                public_only: false,
                context: true,
                fields: Some("id,signature,role,snippet".into()),
            }),
        )
        .await;

        assert_eq!(response.0["total"], 1);
        let result = &response.0["results"][0];
        assert_eq!(result["name"], "main");
        assert_eq!(result["id"], "app::main");
        assert_eq!(result["signature"], "fn main()");
        assert_eq!(result["role"], "entry_point");
        assert_eq!(result["snippet"], "fn main() { helper(); }");
        assert!(result.get("file").is_none());
        assert_eq!(result["calls"][0], "app::helper");
    }

    #[tokio::test]
    async fn standalone_annotation_sync_routes_records_by_project_id() {
        let dir = tempdir().unwrap();
        let state = Arc::new(AnnotationServiceState {
            data_root: dir.path().to_path_buf(),
            log: crate::serve::AnnotationServiceLog::disabled(),
        });
        let response = sync_standalone_annotations(
            State(state.clone()),
            Json(AnnotationSyncRequest {
                project: None,
                annotations: vec![annotation_record("remote-demo", "main", "s:DemoUSR")],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let store = crate::annotations::AnnotationStore::for_project_id_with_data_root(
            "remote-demo",
            dir.path(),
        )
        .unwrap();
        let records = store.list_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].project_id, "remote-demo");
        assert_eq!(records[0].branch, "");
    }

    #[tokio::test]
    async fn standalone_annotation_sync_logs_counts_without_text() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("annotation-service.log");
        let state = Arc::new(AnnotationServiceState {
            data_root: dir.path().to_path_buf(),
            log: crate::serve::AnnotationServiceLog::open(log_file.clone(), false).unwrap(),
        });

        let response = sync_standalone_annotations(
            State(state),
            Json(AnnotationSyncRequest {
                project: None,
                annotations: vec![annotation_record("remote-demo", "main", "s:DemoUSR")],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let log = std::fs::read_to_string(log_file).unwrap();
        assert!(log.contains("annotations sync completed received=1 merged=1"));
        assert!(!log.contains("Explains a reusable invariant."));
    }

    #[tokio::test]
    async fn standalone_annotation_list_requires_project_id() {
        let dir = tempdir().unwrap();
        let state = Arc::new(AnnotationServiceState {
            data_root: dir.path().to_path_buf(),
            log: crate::serve::AnnotationServiceLog::disabled(),
        });
        let response = list_standalone_annotations(
            State(state),
            Query(StandaloneAnnotationListParams { project_id: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
