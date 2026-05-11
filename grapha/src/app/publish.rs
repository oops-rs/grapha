use std::path::PathBuf;

use crate::remote::{ProjectIndexBundle, ProjectRevisionSummary};

use super::index::load_graph_uncached;

pub(crate) fn handle_publish(path: PathBuf, server: String, channel: String) -> anyhow::Result<()> {
    let graph = load_graph_uncached(&path)?;
    let config = crate::config::load_config(&path);
    let bundle = crate::remote::build_publish_bundle(
        &path,
        graph,
        &channel,
        config.index_input_fingerprint(),
    )?;
    let summary = publish_bundle(&server, &bundle)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

pub(crate) fn publish_bundle(
    server: &str,
    bundle: &ProjectIndexBundle,
) -> anyhow::Result<ProjectRevisionSummary> {
    #[derive(serde::Deserialize)]
    struct PublishResponse {
        revision: ProjectRevisionSummary,
    }

    let endpoint = crate::http_client::HttpEndpoint::parse(server)?;
    let project_id = urlencoding::encode(&bundle.metadata.project_id);
    let response: PublishResponse = crate::http_client::post_json(
        &endpoint,
        &format!("/api/projects/{project_id}/revisions"),
        bundle,
    )?;
    Ok(response.revision)
}

#[cfg(test)]
mod tests {
    use grapha_core::graph::Graph;

    #[test]
    fn publish_path_uses_project_id_endpoint() {
        let endpoint =
            crate::http_client::HttpEndpoint::parse("http://127.0.0.1:8080/root").unwrap();
        let project_id = urlencoding::encode("demo.project");

        assert_eq!(
            endpoint.path(&format!("/api/projects/{project_id}/revisions")),
            "/root/api/projects/demo.project/revisions"
        );
    }

    #[test]
    fn build_bundle_uses_requested_channel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("grapha.toml"),
            "[repo]\nproject_id = \"demo-project\"\nname = \"Demo\"\n",
        )
        .unwrap();

        let bundle = crate::remote::build_publish_bundle(
            dir.path(),
            Graph::new(),
            "release/1.0",
            "{}".to_string(),
        )
        .unwrap();

        assert_eq!(bundle.metadata.project_id, "demo-project");
        assert_eq!(bundle.metadata.repo_name, "Demo");
        assert_eq!(bundle.metadata.channel, "release/1.0");
    }
}
