use crate::AnnotationCommands;

fn resolve_sync_server(
    path: &std::path::Path,
    cli_server: Option<String>,
) -> anyhow::Result<String> {
    resolve_sync_server_from(
        cli_server,
        std::env::var("GRAPHA_ANNOTATION_SERVER").ok(),
        crate::config::load_config(path).annotations.server,
    )
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn resolve_sync_server_from(
    cli_server: Option<String>,
    env_server: Option<String>,
    config_server: Option<String>,
) -> anyhow::Result<String> {
    cli_server
        .and_then(non_empty)
        .or_else(|| env_server.and_then(non_empty))
        .or_else(|| config_server.and_then(non_empty))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "annotation server not configured; pass --server, set GRAPHA_ANNOTATION_SERVER, or add [annotations] server = \"http://HOST:8080\" to grapha.toml"
            )
        })
}

pub(crate) fn handle_annotation_command(command: AnnotationCommands) -> anyhow::Result<()> {
    match command {
        AnnotationCommands::Serve { path, port, watch } => {
            crate::app::serve::handle_serve(path, port, false, watch)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_server_prefers_cli_over_env_and_config() {
        let server = resolve_sync_server_from(
            Some(" http://cli:8080 ".to_string()),
            Some("http://env:8080".to_string()),
            Some("http://config:8080".to_string()),
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
        )
        .unwrap();

        assert_eq!(server, "http://config:8080");
    }

    #[test]
    fn sync_server_requires_at_least_one_non_empty_source() {
        let error = resolve_sync_server_from(None, Some(" ".to_string()), Some("".to_string()))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("annotation server not configured")
        );
    }
}
