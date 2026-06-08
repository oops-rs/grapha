use std::collections::HashMap;

use grapha_core::graph::{EdgeKind, Graph, Node};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyQueryOptions {
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyReport {
    pub total: usize,
    pub dependencies: Vec<DependencyRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyRecord {
    pub package: String,
    pub manifest: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_req: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_detail: Option<String>,
    pub kind: String,
    #[serde(skip_serializing_if = "is_false")]
    pub inherited_workspace: bool,
    pub package_id: String,
    pub dependency_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub fn query_dependencies(graph: &Graph, options: &DependencyQueryOptions) -> DependencyReport {
    let node_index: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut dependencies = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::DependsOn)
        .filter_map(|edge| {
            let package_node = node_index.get(edge.source.as_str()).copied()?;
            let dependency_node = node_index.get(edge.target.as_str()).copied()?;
            dependency_record(package_node, dependency_node)
        })
        .filter(|record| matches_name(record, options.name.as_deref()))
        .collect::<Vec<_>>();

    dependencies.sort_by(|left, right| {
        (
            left.repo.as_deref().unwrap_or(""),
            left.manifest.as_str(),
            dependency_kind_rank(&left.kind),
            left.name.as_str(),
        )
            .cmp(&(
                right.repo.as_deref().unwrap_or(""),
                right.manifest.as_str(),
                dependency_kind_rank(&right.kind),
                right.name.as_str(),
            ))
    });

    DependencyReport {
        total: dependencies.len(),
        dependencies,
    }
}

fn dependency_record(package_node: &Node, dependency_node: &Node) -> Option<DependencyRecord> {
    if package_node.metadata.get("cargo.node").map(String::as_str) != Some("package")
        || dependency_node
            .metadata
            .get("cargo.node")
            .map(String::as_str)
            != Some("dependency")
    {
        return None;
    }

    let metadata = &dependency_node.metadata;
    let name = metadata
        .get("cargo.dependency.name")
        .cloned()
        .unwrap_or_else(|| dependency_node.name.clone());
    Some(DependencyRecord {
        package: metadata
            .get("cargo.package.name")
            .cloned()
            .or_else(|| package_node.metadata.get("cargo.package.name").cloned())
            .unwrap_or_else(|| package_node.name.clone()),
        manifest: metadata
            .get("cargo.manifest_path")
            .cloned()
            .unwrap_or_else(|| dependency_node.file.to_string_lossy().replace('\\', "/")),
        name,
        package_name: metadata.get("cargo.dependency.package").cloned(),
        version_req: metadata.get("cargo.dependency.version_req").cloned(),
        source: metadata
            .get("cargo.dependency.source")
            .cloned()
            .unwrap_or_else(|| "registry".to_string()),
        source_detail: metadata.get("cargo.dependency.source_detail").cloned(),
        kind: metadata
            .get("cargo.dependency.kind")
            .cloned()
            .unwrap_or_else(|| {
                dependency_node
                    .id
                    .split("::cargo_dependency::")
                    .nth(1)
                    .and_then(|tail| tail.split("::").next())
                    .unwrap_or("normal")
                    .to_string()
            }),
        inherited_workspace: metadata
            .get("cargo.dependency.inherited_workspace")
            .is_some_and(|value| value == "true"),
        package_id: package_node.id.clone(),
        dependency_id: dependency_node.id.clone(),
        repo: dependency_node
            .repo
            .clone()
            .or_else(|| package_node.repo.clone()),
    })
}

fn matches_name(record: &DependencyRecord, query: Option<&str>) -> bool {
    let Some(query) = query else {
        return true;
    };
    record.name == query || record.package_name.as_deref() == Some(query)
}

fn dependency_kind_rank(kind: &str) -> usize {
    match kind {
        "normal" => 0,
        "dev" => 1,
        "build" => 2,
        "workspace" => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, Graph, Node, NodeKind, Span, Visibility};
    use std::collections::HashMap;

    fn package_node() -> Node {
        let mut metadata = HashMap::new();
        metadata.insert("cargo.node".to_string(), "package".to_string());
        metadata.insert("cargo.package.name".to_string(), "app".to_string());
        metadata.insert("cargo.manifest_path".to_string(), "Cargo.toml".to_string());
        Node {
            id: "Cargo.toml::cargo_package".to_string(),
            kind: NodeKind::Package,
            name: "app".to_string(),
            file: "Cargo.toml".into(),
            span: Span {
                start: [1, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata,
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("app".to_string()),
            snippet: None,
            repo: Some("repo".to_string()),
        }
    }

    fn dependency_node(name: &str, package_name: Option<&str>) -> Node {
        let mut metadata = HashMap::new();
        metadata.insert("cargo.node".to_string(), "dependency".to_string());
        metadata.insert("cargo.package.name".to_string(), "app".to_string());
        metadata.insert("cargo.manifest_path".to_string(), "Cargo.toml".to_string());
        metadata.insert("cargo.dependency.name".to_string(), name.to_string());
        metadata.insert("cargo.dependency.kind".to_string(), "normal".to_string());
        metadata.insert(
            "cargo.dependency.version_req".to_string(),
            "0.7".to_string(),
        );
        metadata.insert(
            "cargo.dependency.source".to_string(),
            "registry".to_string(),
        );
        if let Some(package_name) = package_name {
            metadata.insert(
                "cargo.dependency.package".to_string(),
                package_name.to_string(),
            );
        }
        Node {
            id: format!("Cargo.toml::cargo_dependency::normal::{name}"),
            kind: NodeKind::Package,
            name: name.to_string(),
            file: "Cargo.toml".into(),
            span: Span {
                start: [1, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata,
            role: None,
            signature: Some("0.7".to_string()),
            doc_comment: None,
            module: Some("app".to_string()),
            snippet: None,
            repo: Some("repo".to_string()),
        }
    }

    #[test]
    fn reports_dependency_declarations_from_depends_on_edges() {
        let package = package_node();
        let dependency = dependency_node("mentra", None);
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![package.clone(), dependency.clone()],
            edges: vec![Edge {
                source: package.id,
                target: dependency.id,
                kind: EdgeKind::DependsOn,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: Some("repo".to_string()),
            }],
        };

        let report = query_dependencies(&graph, &DependencyQueryOptions::default());

        assert_eq!(report.total, 1);
        assert_eq!(report.dependencies[0].name, "mentra");
        assert_eq!(report.dependencies[0].version_req.as_deref(), Some("0.7"));
        assert_eq!(report.dependencies[0].source, "registry");
        assert_eq!(report.dependencies[0].kind, "normal");
    }

    #[test]
    fn filters_by_declared_or_renamed_package_name() {
        let package = package_node();
        let dependency = dependency_node("serde_json_alias", Some("serde_json"));
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![package.clone(), dependency.clone()],
            edges: vec![Edge {
                source: package.id,
                target: dependency.id,
                kind: EdgeKind::DependsOn,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };

        let report = query_dependencies(
            &graph,
            &DependencyQueryOptions {
                name: Some("serde_json".to_string()),
            },
        );

        assert_eq!(report.total, 1);
        assert_eq!(report.dependencies[0].name, "serde_json_alias");
    }
}
