use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::annotations::{AnnotationStore, SymbolAnnotationRecord};
use crate::data_paths::ProjectIdentity;

#[derive(Debug, Serialize)]
pub struct AnnotationSyncReport {
    pub pulled: usize,
    pub pushed: usize,
    pub local_total: usize,
    pub remote_total: usize,
}

#[derive(Debug, Deserialize)]
struct AnnotationListResponse {
    annotations: Vec<SymbolAnnotationRecord>,
}

#[derive(Debug, Deserialize)]
struct AnnotationMergeResponse {
    merged: usize,
}

#[derive(Debug)]
struct HttpEndpoint {
    host: String,
    port: u16,
    path_prefix: String,
}

impl HttpEndpoint {
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let without_scheme = raw
            .trim()
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("only http:// annotation sync URLs are supported"))?;
        let (authority, path_prefix) = without_scheme
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((without_scheme, String::new()));
        if authority.is_empty() {
            anyhow::bail!("annotation sync URL is missing a host");
        }
        let (host, port) = authority
            .rsplit_once(':')
            .map(|(host, port)| {
                let parsed_port = port
                    .parse::<u16>()
                    .map_err(|_| anyhow::anyhow!("invalid annotation sync port: {port}"))?;
                Ok::<_, anyhow::Error>((host.to_string(), parsed_port))
            })
            .transpose()?
            .unwrap_or_else(|| (authority.to_string(), 80));
        if host.is_empty() {
            anyhow::bail!("annotation sync URL is missing a host");
        }
        Ok(Self {
            host,
            port,
            path_prefix: path_prefix.trim_end_matches('/').to_string(),
        })
    }

    fn path(&self, path: &str) -> String {
        format!("{}{}", self.path_prefix, path)
    }
}

pub fn sync_annotations(project_root: &Path, server: &str) -> anyhow::Result<AnnotationSyncReport> {
    let endpoint = HttpEndpoint::parse(server)?;
    let project = crate::data_paths::project_identity(project_root);
    let store = AnnotationStore::for_project_root(project_root);
    let remote_before = fetch_remote_annotations(&endpoint, &project.project_id)?;
    let remote_total = remote_before.len();
    let pulled = store.merge_records(&remote_before)?;
    let local_after = store.list_records()?;
    let pushed = push_remote_annotations(&endpoint, &project, &local_after)?;

    Ok(AnnotationSyncReport {
        pulled,
        pushed,
        local_total: local_after.len(),
        remote_total,
    })
}

fn fetch_remote_annotations(
    endpoint: &HttpEndpoint,
    project_id: &str,
) -> anyhow::Result<Vec<SymbolAnnotationRecord>> {
    let path = format!(
        "/api/annotations?project_id={}",
        urlencoding::encode(project_id)
    );
    let body = request(endpoint, "GET", &path, None)?;
    let response: AnnotationListResponse = serde_json::from_str(&body)?;
    Ok(response.annotations)
}

fn push_remote_annotations(
    endpoint: &HttpEndpoint,
    project: &ProjectIdentity,
    records: &[SymbolAnnotationRecord],
) -> anyhow::Result<usize> {
    let payload = serde_json::json!({
        "project": project,
        "annotations": records
    });
    let body = request(
        endpoint,
        "POST",
        "/api/annotations/sync",
        Some(&serde_json::to_vec(&payload)?),
    )?;
    let response: AnnotationMergeResponse = serde_json::from_str(&body)?;
    Ok(response.merged)
}

fn request(
    endpoint: &HttpEndpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> anyhow::Result<String> {
    let body = body.unwrap_or(&[]);
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;
    let request_path = endpoint.path(path);
    write!(
        stream,
        "{method} {request_path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        endpoint.host,
        body.len()
    )?;
    stream.write_all(body)?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let response = String::from_utf8(response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response from annotation server"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP status from annotation server"))?;
    if !(200..300).contains(&status) {
        anyhow::bail!("annotation server returned HTTP {status}: {body}");
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_http_endpoint() {
        let endpoint = HttpEndpoint::parse("http://127.0.0.1:8080/grapha").unwrap();

        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8080);
        assert_eq!(endpoint.path("/api/annotations"), "/grapha/api/annotations");
    }

    #[test]
    fn rejects_https_until_tls_client_exists() {
        let error = HttpEndpoint::parse("https://example.com").unwrap_err();

        assert!(error.to_string().contains("only http://"));
    }
}
