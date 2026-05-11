use std::path::PathBuf;

use crate::serve;

use super::index::{load_graph, open_search_index};

const DEFAULT_SERVE_HOST: &str = "0.0.0.0";
const DEFAULT_SERVE_PORT: u16 = 8080;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeOptions {
    host: String,
    port: u16,
}

fn resolve_serve_options(
    path: &std::path::Path,
    cli_host: Option<String>,
    cli_port: Option<u16>,
) -> ServeOptions {
    let project = crate::config::load_config(path).serve;
    let global = crate::config::load_global_config().serve;
    resolve_serve_options_from(cli_host, cli_port, project, global)
}

fn resolve_serve_options_from(
    cli_host: Option<String>,
    cli_port: Option<u16>,
    project: crate::config::ServeConfig,
    global: crate::config::ServeConfig,
) -> ServeOptions {
    let host = cli_host
        .and_then(non_empty)
        .or_else(|| project.host.and_then(non_empty))
        .or_else(|| global.host.and_then(non_empty))
        .unwrap_or_else(|| DEFAULT_SERVE_HOST.to_string());
    let port = cli_port
        .or(project.port)
        .or(global.port)
        .unwrap_or(DEFAULT_SERVE_PORT);

    ServeOptions { host, port }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn handle_serve(
    path: PathBuf,
    host: Option<String>,
    port: Option<u16>,
    verbose: bool,
) -> anyhow::Result<()> {
    let options = resolve_serve_options(&path, host, port);
    let graph = load_graph(&path)?;
    let search_index = open_search_index(&path, verbose)?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(serve::run(
        path,
        graph,
        search_index,
        options.host,
        options.port,
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_config(
        host: Option<&str>,
        port: Option<u16>,
        watch: Option<bool>,
    ) -> crate::config::ServeConfig {
        crate::config::ServeConfig {
            host: host.map(str::to_string),
            port,
            watch,
        }
    }

    #[test]
    fn serve_options_prefer_cli_over_project_and_global() {
        let options = resolve_serve_options_from(
            Some(" 127.0.0.1 ".to_string()),
            Some(19090),
            serve_config(Some("0.0.0.0"), Some(18081), Some(true)),
            serve_config(Some("localhost"), Some(18080), Some(true)),
        );

        assert_eq!(
            options,
            ServeOptions {
                host: "127.0.0.1".to_string(),
                port: 19090
            }
        );
    }

    #[test]
    fn serve_options_prefer_project_over_global() {
        let options = resolve_serve_options_from(
            None,
            None,
            serve_config(Some("127.0.0.1"), Some(18081), Some(true)),
            serve_config(Some("0.0.0.0"), Some(18080), Some(false)),
        );

        assert_eq!(
            options,
            ServeOptions {
                host: "127.0.0.1".to_string(),
                port: 18081
            }
        );
    }

    #[test]
    fn serve_options_fall_back_to_global_then_defaults() {
        let options = resolve_serve_options_from(
            Some(" ".to_string()),
            None,
            serve_config(Some("\t"), None, None),
            serve_config(None, Some(18080), Some(true)),
        );

        assert_eq!(
            options,
            ServeOptions {
                host: DEFAULT_SERVE_HOST.to_string(),
                port: 18080
            }
        );
    }
}
