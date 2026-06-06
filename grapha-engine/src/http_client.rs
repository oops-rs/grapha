use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEndpoint {
    host: String,
    port: u16,
    path_prefix: String,
}

impl HttpEndpoint {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let without_scheme = raw
            .trim()
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("only http:// Grapha server URLs are supported"))?;
        let (authority, path_prefix) = without_scheme
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((without_scheme, String::new()));
        if authority.is_empty() {
            anyhow::bail!("Grapha server URL is missing a host");
        }
        let (host, port) = authority
            .rsplit_once(':')
            .map(|(host, port)| {
                let parsed_port = port
                    .parse::<u16>()
                    .map_err(|_| anyhow::anyhow!("invalid Grapha server port: {port}"))?;
                Ok::<_, anyhow::Error>((host.to_string(), parsed_port))
            })
            .transpose()?
            .unwrap_or_else(|| (authority.to_string(), 80));
        if host.is_empty() {
            anyhow::bail!("Grapha server URL is missing a host");
        }
        Ok(Self {
            host,
            port,
            path_prefix: path_prefix.trim_end_matches('/').to_string(),
        })
    }

    pub fn path(&self, path: &str) -> String {
        format!("{}{}", self.path_prefix, path)
    }
}

pub fn request(
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
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response from Grapha server"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP status from Grapha server"))?;
    if !(200..300).contains(&status) {
        anyhow::bail!("Grapha server returned HTTP {status}: {body}");
    }
    Ok(body.to_string())
}

pub fn get_json<T: DeserializeOwned>(endpoint: &HttpEndpoint, path: &str) -> anyhow::Result<T> {
    let body = request(endpoint, "GET", path, None)?;
    Ok(serde_json::from_str(&body)?)
}

pub fn post_json<T: DeserializeOwned, P: Serialize>(
    endpoint: &HttpEndpoint,
    path: &str,
    payload: &P,
) -> anyhow::Result<T> {
    let body = request(endpoint, "POST", path, Some(&serde_json::to_vec(payload)?))?;
    Ok(serde_json::from_str(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_http_endpoint() {
        let endpoint = HttpEndpoint::parse("http://127.0.0.1:8080/grapha").unwrap();

        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8080);
        assert_eq!(endpoint.path("/api/projects"), "/grapha/api/projects");
    }

    #[test]
    fn rejects_https_until_tls_client_exists() {
        let error = HttpEndpoint::parse("https://example.com").unwrap_err();

        assert!(error.to_string().contains("only http://"));
    }
}
