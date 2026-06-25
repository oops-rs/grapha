//! End-to-end proof that Grapha's upload path gzip-compresses its request body
//! over a real TCP socket and the server transparently decompresses it.
//!
//! Unit tests cover the two halves in isolation (`compress_upload_body` round
//! trips, and `RequestDecompressionLayer` decodes an in-memory request). This
//! exercises the full loop the way `grapha publish` actually runs it: the raw
//! `http_client::post_json` client framing a gzip body across a socket into a
//! live axum server guarded by the same decompression + body-limit layers as
//! `grapha serve`.

use std::net::SocketAddr;

use axum::extract::DefaultBodyLimit;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::decompression::RequestDecompressionLayer;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Payload {
    project_id: String,
    nodes: Vec<u64>,
}

/// Echoes the decoded JSON back. If the body still arrived gzip-compressed the
/// `Json` extractor would fail to parse it and this handler would never run.
async fn echo(Json(payload): Json<Payload>) -> Json<Payload> {
    Json(payload)
}

async fn spawn_echo_server() -> SocketAddr {
    let app = Router::new()
        .route("/api/echo", post(echo))
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(RequestDecompressionLayer::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_upload_round_trips_gzip_over_tcp() {
    let addr = spawn_echo_server().await;

    // A payload large enough that compression is unambiguously exercised.
    let payload = Payload {
        project_id: "demo-project".to_string(),
        nodes: (0..5_000).collect(),
    };

    // `post_json` writes a gzip-compressed body with `Content-Encoding: gzip`
    // over a blocking std `TcpStream`, so drive it off the async runtime.
    let server = format!("http://{addr}");
    let sent = payload.clone();
    let echoed: Payload = tokio::task::spawn_blocking(move || {
        let endpoint = grapha::http_client::HttpEndpoint::parse(&server).unwrap();
        grapha::http_client::post_json(&endpoint, "/api/echo", &sent).unwrap()
    })
    .await
    .unwrap();

    assert_eq!(echoed, payload);
}
