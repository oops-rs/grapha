use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

use crate::AnnotationCommands;

const DAEMON_LOG_STDERR_ENV: &str = "GRAPHA_ANNOTATION_LOG_STDERR";

fn resolve_sync_server(
    path: &std::path::Path,
    cli_server: Option<String>,
) -> anyhow::Result<String> {
    resolve_sync_server_from(
        cli_server,
        std::env::var("GRAPHA_ANNOTATION_SERVER").ok(),
        crate::config::load_config(path).annotations.server,
        crate::config::load_global_config().annotations.server,
    )
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn resolve_sync_server_from(
    cli_server: Option<String>,
    env_server: Option<String>,
    project_server: Option<String>,
    global_server: Option<String>,
) -> anyhow::Result<String> {
    cli_server
        .and_then(non_empty)
        .or_else(|| env_server.and_then(non_empty))
        .or_else(|| project_server.and_then(non_empty))
        .or_else(|| global_server.and_then(non_empty))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "annotation server not configured; pass --server, set GRAPHA_ANNOTATION_SERVER, or add [annotations] server = \"http://HOST:8080\" to grapha.toml or the global Grapha config"
            )
        })
}

pub(crate) fn handle_annotation_command(command: AnnotationCommands) -> anyhow::Result<()> {
    match command {
        AnnotationCommands::Serve {
            path: _,
            port,
            log_file,
            daemon,
            watch,
        } => {
            if watch {
                eprintln!("annotation service is standalone; --watch is accepted but ignored");
            }
            let log_file = log_file.unwrap_or_else(crate::config::default_annotation_log_path);
            if daemon {
                return spawn_annotation_service_daemon(port, &log_file);
            }
            let rt = tokio::runtime::Runtime::new()?;
            let mirror_stderr = std::env::var(DAEMON_LOG_STDERR_ENV)
                .map(|value| value != "0")
                .unwrap_or(true);
            rt.block_on(crate::serve::run_annotation_service(
                port,
                log_file,
                mirror_stderr,
            ))?;
            Ok(())
        }
        AnnotationCommands::Sync { server, path } => {
            let server = resolve_sync_server(&path, server)?;
            let report = crate::annotation_sync::sync_annotations(&path, &server)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        AnnotationCommands::List { path } => {
            let records =
                crate::annotations::AnnotationStore::for_project_root(&path).list_records()?;
            let total = records.len();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "project": crate::data_paths::project_identity(&path),
                    "annotations": records,
                    "total": total
                }))?
            );
            Ok(())
        }
    }
}

fn spawn_annotation_service_daemon(port: u16, log_file: &Path) -> anyhow::Result<()> {
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "creating annotation service log directory {}",
                parent.display()
            )
        })?;
    }

    append_daemon_log_line(
        log_file,
        &format!("starting annotation service daemon on 0.0.0.0:{port}"),
    )?;

    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("opening annotation service log file {}", log_file.display()))?;
    let stderr = stdout
        .try_clone()
        .context("cloning annotation service log file handle")?;
    let executable = std::env::current_exe().context("locating current grapha executable")?;
    let child = Command::new(executable)
        .arg("annotation")
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .arg("--log-file")
        .arg(log_file)
        .env(DAEMON_LOG_STDERR_ENV, "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("starting annotation service daemon")?;

    append_daemon_log_line(
        log_file,
        &format!(
            "annotation service daemon started pid={} port={port}",
            child.id()
        ),
    )?;
    eprintln!(
        "  \x1b[32m✓\x1b[0m annotation service daemon started at http://localhost:{port} (pid {}, log {})",
        child.id(),
        log_file.display()
    );
    Ok(())
}

fn append_daemon_log_line(log_file: &Path, message: &str) -> anyhow::Result<()> {
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "creating annotation service log directory {}",
                parent.display()
            )
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("opening annotation service log file {}", log_file.display()))?;
    use std::io::Write;
    writeln!(file, "{} {message}", annotation_log_timestamp())?;
    Ok(())
}

fn annotation_log_timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("[unix_ms={millis}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_server_prefers_cli_over_env_and_config() {
        let server = resolve_sync_server_from(
            Some(" http://cli:8080 ".to_string()),
            Some("http://env:8080".to_string()),
            Some("http://config:8080".to_string()),
            Some("http://global:8080".to_string()),
        )
        .unwrap();

        assert_eq!(server, "http://cli:8080");
    }

    #[test]
    fn sync_server_uses_env_before_config() {
        let server = resolve_sync_server_from(
            None,
            Some(" http://env:8080 ".to_string()),
            Some("http://config:8080".to_string()),
            Some("http://global:8080".to_string()),
        )
        .unwrap();

        assert_eq!(server, "http://env:8080");
    }

    #[test]
    fn sync_server_uses_config_when_cli_and_env_are_empty() {
        let server = resolve_sync_server_from(
            Some(" ".to_string()),
            Some("\t".to_string()),
            Some(" http://config:8080 ".to_string()),
            Some("http://global:8080".to_string()),
        )
        .unwrap();

        assert_eq!(server, "http://config:8080");
    }

    #[test]
    fn sync_server_uses_global_config_when_other_sources_are_empty() {
        let server = resolve_sync_server_from(
            None,
            Some(" ".to_string()),
            Some("\t".to_string()),
            Some(" http://global:8080 ".to_string()),
        )
        .unwrap();

        assert_eq!(server, "http://global:8080");
    }

    #[test]
    fn sync_server_requires_at_least_one_non_empty_source() {
        let error = resolve_sync_server_from(
            None,
            Some(" ".to_string()),
            Some("".to_string()),
            Some("\n".to_string()),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("annotation server not configured")
        );
    }

    #[test]
    fn daemon_log_line_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let log_file = dir.path().join("config").join("annotation-service.log");

        append_daemon_log_line(&log_file, "daemon test").unwrap();

        let contents = std::fs::read_to_string(log_file).unwrap();
        assert!(contents.contains("daemon test"));
    }
}
