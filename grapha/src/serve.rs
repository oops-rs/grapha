pub mod api;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::response::Html;
use axum::routing::{get, post};

use grapha_core::graph::Graph;
use tantivy::Index;

const INDEX_HTML: &str = include_str!("serve/web/index.html");

pub struct AppState {
    pub project_path: PathBuf,
    pub graph: Graph,
    pub search_index: Index,
}

pub struct AnnotationServiceState {
    pub data_root: PathBuf,
    pub log: AnnotationServiceLog,
}

#[derive(Clone)]
pub struct AnnotationServiceLog {
    file: Option<Arc<Mutex<File>>>,
    mirror_stderr: bool,
}

impl AnnotationServiceLog {
    pub fn open(path: PathBuf, mirror_stderr: bool) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Some(Arc::new(Mutex::new(file))),
            mirror_stderr,
        })
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            file: None,
            mirror_stderr: false,
        }
    }

    pub fn event(&self, message: impl AsRef<str>) {
        let line = format!("{} {}\n", annotation_log_timestamp(), message.as_ref());
        if self.mirror_stderr {
            eprint!("{line}");
        }
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

pub async fn run(
    project_path: PathBuf,
    graph: Graph,
    search_index: Index,
    host: String,
    port: u16,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        project_path,
        graph,
        search_index,
    });
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/graph", get(api::get_graph))
        .route("/api/entries", get(api::get_entries))
        .route("/api/context/{symbol}", get(api::get_context))
        .route("/api/trace/{symbol}", get(api::get_trace))
        .route("/api/reverse/{symbol}", get(api::get_reverse))
        .route("/api/status", get(api::get_index_status))
        .route("/api/search", get(api::get_search))
        .route(
            "/api/annotations",
            get(api::list_annotations).post(api::post_annotation),
        )
        .route(
            "/api/annotations/sync",
            axum::routing::post(api::sync_annotations),
        )
        .route("/api/annotations/{symbol}", get(api::get_annotation))
        .with_state(state)
        .layer(tower_http::cors::CorsLayer::permissive());

    eprintln!("  \x1b[32m✓\x1b[0m serving at http://{host}:{port}");
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub async fn run_annotation_service(
    port: u16,
    log_path: PathBuf,
    mirror_stderr: bool,
) -> anyhow::Result<()> {
    let data_root = crate::data_paths::global_data_root();
    let log = AnnotationServiceLog::open(log_path.clone(), mirror_stderr)?;
    log.event(format!(
        "annotation service starting bind=0.0.0.0:{port} data_root={} log_file={}",
        data_root.display(),
        log_path.display()
    ));
    let state = Arc::new(AnnotationServiceState { data_root, log });
    let app = Router::new()
        .route("/", get(|| async { Html("Grapha annotation service") }))
        .route("/api/annotations", get(api::list_standalone_annotations))
        .route(
            "/api/annotations/sync",
            post(api::sync_standalone_annotations),
        )
        .with_state(state)
        .layer(tower_http::cors::CorsLayer::permissive());

    eprintln!(
        "  \x1b[32m✓\x1b[0m annotation service at http://localhost:{port} (log {})",
        log_path.display()
    );
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn annotation_log_timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("[unix_ms={millis}]")
}
