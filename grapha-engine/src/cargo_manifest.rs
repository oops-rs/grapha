use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use grapha_core::ExtractionResult;
use grapha_core::graph::{Edge, EdgeKind, EdgeProvenance, Node, NodeKind, Span, Visibility};
use toml::{Table, Value};

const DEP_METADATA_PREFIX: &str = "cargo.dependency.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CargoDependencyKind {
    Normal,
    Dev,
    Build,
    Workspace,
}

impl CargoDependencyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Dev => "dev",
            Self::Build => "build",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoDependencyDeclaration {
    pub manifest_path: PathBuf,
    pub package: String,
    pub name: String,
    pub package_name: Option<String>,
    pub version_req: Option<String>,
    pub source: String,
    pub source_detail: Option<String>,
    pub kind: CargoDependencyKind,
    pub inherited_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDependency {
    name: String,
    package_name: Option<String>,
    version_req: Option<String>,
    source: String,
    source_detail: Option<String>,
    workspace: bool,
}

pub fn parse_manifest_table(content: &str) -> Result<Table, toml::de::Error> {
    content.parse::<Table>()
}

pub fn read_manifest_table(path: &Path) -> anyhow::Result<Table> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_manifest_table(&content)?)
}

pub fn package_name_from_table(table: &Table, fallback_dir: &Path) -> String {
    table
        .get("package")
        .and_then(Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(Value::as_str)
        .or_else(|| fallback_dir.file_name().and_then(|name| name.to_str()))
        .unwrap_or("root")
        .to_string()
}

pub fn workspace_member_paths_from_table(root: &Path, table: &Table) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Some(workspace) = table.get("workspace").and_then(Value::as_table) else {
        return paths;
    };
    let Some(members) = workspace.get("members").and_then(Value::as_array) else {
        return paths;
    };

    for member in members {
        if let Some(pattern) = member.as_str() {
            expand_workspace_member_paths(root, pattern, &mut paths);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

pub fn discover_cargo_manifest_paths(root: &Path) -> Vec<PathBuf> {
    let root_manifest = root.join("Cargo.toml");
    if !root_manifest.is_file() {
        return Vec::new();
    }

    let mut manifests = vec![root_manifest.clone()];
    if let Ok(table) = read_manifest_table(&root_manifest) {
        for member_path in workspace_member_paths_from_table(root, &table) {
            let manifest = member_path.join("Cargo.toml");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    manifests.sort();
    manifests.dedup();
    manifests
}

pub fn dependency_metadata_key(suffix: &str) -> String {
    format!("{DEP_METADATA_PREFIX}{suffix}")
}

pub fn extract_dependency_graph(root: &Path) -> ExtractionResult {
    let manifest_paths = discover_cargo_manifest_paths(root);
    if manifest_paths.is_empty() {
        return ExtractionResult::new();
    }

    let root_table = read_manifest_table(&root.join("Cargo.toml")).ok();
    let workspace_dependencies = root_table
        .as_ref()
        .map(parse_workspace_dependencies)
        .unwrap_or_default();

    let mut packages = BTreeMap::new();
    let mut declarations = Vec::new();
    for manifest_path in manifest_paths {
        let Ok(table) = read_manifest_table(&manifest_path) else {
            continue;
        };
        let manifest_rel = relative_manifest_path(root, &manifest_path);
        let package = package_name_from_table(
            &table,
            manifest_path.parent().unwrap_or_else(|| Path::new("")),
        );
        packages.insert(manifest_rel, package);
        declarations.extend(declarations_from_table(
            root,
            &manifest_path,
            &table,
            &workspace_dependencies,
        ));
    }

    declarations_to_graph(root, packages, declarations)
}

fn expand_workspace_member_paths(root: &Path, pattern: &str, paths: &mut Vec<PathBuf>) {
    if pattern.contains('*') {
        let prefix = pattern.trim_end_matches('*').trim_end_matches('/');
        let parent_dir = root.join(prefix);
        if parent_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&parent_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    paths.push(path);
                }
            }
        }
    } else {
        let member_path = root.join(pattern);
        if member_path.is_dir() {
            paths.push(member_path);
        }
    }
}

fn parse_workspace_dependencies(table: &Table) -> BTreeMap<String, ParsedDependency> {
    table
        .get("workspace")
        .and_then(Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Value::as_table)
        .map(parse_dependency_table)
        .unwrap_or_default()
}

fn declarations_from_table(
    root: &Path,
    manifest_path: &Path,
    table: &Table,
    workspace_dependencies: &BTreeMap<String, ParsedDependency>,
) -> Vec<CargoDependencyDeclaration> {
    let package = package_name_from_table(
        table,
        manifest_path.parent().unwrap_or_else(|| Path::new("")),
    );
    let manifest_rel = relative_manifest_path(root, manifest_path);
    let mut declarations = Vec::new();

    if manifest_path == root.join("Cargo.toml") {
        declarations.extend(workspace_dependencies.values().map(|dependency| {
            declaration_from_dependency(
                manifest_rel.clone(),
                package.clone(),
                dependency.clone(),
                CargoDependencyKind::Workspace,
                false,
            )
        }));
    }

    for (section, kind) in [
        ("dependencies", CargoDependencyKind::Normal),
        ("dev-dependencies", CargoDependencyKind::Dev),
        ("build-dependencies", CargoDependencyKind::Build),
    ] {
        let Some(dependencies) = table.get(section).and_then(Value::as_table) else {
            continue;
        };
        for dependency in parse_dependency_table(dependencies).into_values() {
            let (dependency, inherited_workspace) = if dependency.workspace {
                let merged = workspace_dependencies
                    .get(&dependency.name)
                    .map(|workspace| merge_workspace_dependency(dependency.clone(), workspace))
                    .unwrap_or_else(|| dependency.clone());
                (merged, true)
            } else {
                (dependency, false)
            };
            declarations.push(declaration_from_dependency(
                manifest_rel.clone(),
                package.clone(),
                dependency,
                kind,
                inherited_workspace,
            ));
        }
    }

    declarations
}

fn parse_dependency_table(table: &Table) -> BTreeMap<String, ParsedDependency> {
    table
        .iter()
        .map(|(name, value)| (name.clone(), parse_dependency(name, value)))
        .collect()
}

fn parse_dependency(name: &str, value: &Value) -> ParsedDependency {
    match value {
        Value::String(version) => ParsedDependency {
            name: name.to_string(),
            package_name: None,
            version_req: Some(version.clone()),
            source: "registry".to_string(),
            source_detail: None,
            workspace: false,
        },
        Value::Table(table) => {
            let path = table
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string);
            let git = table.get("git").and_then(Value::as_str).map(str::to_string);
            let registry = table
                .get("registry")
                .and_then(Value::as_str)
                .map(str::to_string);
            let (source, source_detail) = if let Some(path) = path {
                ("path".to_string(), Some(path))
            } else if let Some(git) = git {
                ("git".to_string(), Some(git))
            } else {
                ("registry".to_string(), registry)
            };

            ParsedDependency {
                name: name.to_string(),
                package_name: table
                    .get("package")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                version_req: table
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                source,
                source_detail,
                workspace: table
                    .get("workspace")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }
        }
        _ => ParsedDependency {
            name: name.to_string(),
            package_name: None,
            version_req: None,
            source: "registry".to_string(),
            source_detail: None,
            workspace: false,
        },
    }
}

fn merge_workspace_dependency(
    mut dependency: ParsedDependency,
    workspace: &ParsedDependency,
) -> ParsedDependency {
    if dependency.package_name.is_none() {
        dependency.package_name = workspace.package_name.clone();
    }
    if dependency.version_req.is_none() {
        dependency.version_req = workspace.version_req.clone();
    }
    if dependency.source == "registry" && dependency.source_detail.is_none() {
        dependency.source = workspace.source.clone();
        dependency.source_detail = workspace.source_detail.clone();
    }
    dependency
}

fn declaration_from_dependency(
    manifest_path: PathBuf,
    package: String,
    dependency: ParsedDependency,
    kind: CargoDependencyKind,
    inherited_workspace: bool,
) -> CargoDependencyDeclaration {
    CargoDependencyDeclaration {
        manifest_path,
        package,
        name: dependency.name,
        package_name: dependency.package_name,
        version_req: dependency.version_req,
        source: dependency.source,
        source_detail: dependency.source_detail,
        kind,
        inherited_workspace,
    }
}

fn declarations_to_graph(
    root: &Path,
    packages: BTreeMap<PathBuf, String>,
    declarations: Vec<CargoDependencyDeclaration>,
) -> ExtractionResult {
    let mut result = ExtractionResult::new();
    if packages.is_empty() {
        return result;
    }

    let mut package_nodes = BTreeSet::new();
    for (manifest_path, package) in packages {
        let package_id = package_node_id(&manifest_path);
        let span = manifest_span(root, &manifest_path);
        package_nodes.insert(package_id.clone());
        result
            .nodes
            .push(package_node(&package_id, &manifest_path, &package, span));
    }

    for declaration in declarations {
        let package_id = package_node_id(&declaration.manifest_path);
        if package_nodes.insert(package_id.clone()) {
            let span = manifest_span(root, &declaration.manifest_path);
            result.nodes.push(package_node(
                &package_id,
                &declaration.manifest_path,
                &declaration.package,
                span,
            ));
        }

        let dependency_id = dependency_node_id(
            &declaration.manifest_path,
            declaration.kind,
            &declaration.name,
        );
        result
            .nodes
            .push(dependency_node(&dependency_id, &declaration, root));
        result.edges.push(dependency_edge(
            package_id,
            dependency_id,
            &declaration,
            root,
        ));
    }
    result
}

fn package_node(id: &str, manifest_path: &Path, package: &str, span: Span) -> Node {
    let mut metadata = HashMap::new();
    metadata.insert("cargo.node".to_string(), "package".to_string());
    metadata.insert(
        "cargo.manifest_path".to_string(),
        normalize_path(manifest_path),
    );
    metadata.insert("cargo.package.name".to_string(), package.to_string());

    Node {
        id: id.to_string(),
        kind: NodeKind::Package,
        name: package.to_string(),
        file: manifest_path.to_path_buf(),
        span,
        visibility: Visibility::Public,
        metadata,
        role: None,
        signature: None,
        doc_comment: None,
        module: Some(package.to_string()),
        snippet: None,
        repo: None,
    }
}

fn dependency_node(id: &str, declaration: &CargoDependencyDeclaration, root: &Path) -> Node {
    let mut metadata = HashMap::new();
    metadata.insert("cargo.node".to_string(), "dependency".to_string());
    metadata.insert(
        "cargo.manifest_path".to_string(),
        normalize_path(&declaration.manifest_path),
    );
    metadata.insert(
        "cargo.package.name".to_string(),
        declaration.package.clone(),
    );
    metadata.insert(dependency_metadata_key("name"), declaration.name.clone());
    metadata.insert(
        dependency_metadata_key("kind"),
        declaration.kind.as_str().to_string(),
    );
    metadata.insert(
        dependency_metadata_key("source"),
        declaration.source.clone(),
    );
    metadata.insert(
        dependency_metadata_key("inherited_workspace"),
        declaration.inherited_workspace.to_string(),
    );
    if let Some(package_name) = &declaration.package_name {
        metadata.insert(dependency_metadata_key("package"), package_name.clone());
    }
    if let Some(version_req) = &declaration.version_req {
        metadata.insert(dependency_metadata_key("version_req"), version_req.clone());
    }
    if let Some(source_detail) = &declaration.source_detail {
        metadata.insert(
            dependency_metadata_key("source_detail"),
            source_detail.clone(),
        );
    }

    Node {
        id: id.to_string(),
        kind: NodeKind::Package,
        name: declaration.name.clone(),
        file: declaration.manifest_path.clone(),
        span: manifest_span(root, &declaration.manifest_path),
        visibility: Visibility::Public,
        metadata,
        role: None,
        signature: declaration.version_req.clone(),
        doc_comment: None,
        module: Some(declaration.package.clone()),
        snippet: None,
        repo: None,
    }
}

fn dependency_edge(
    source: String,
    target: String,
    declaration: &CargoDependencyDeclaration,
    root: &Path,
) -> Edge {
    Edge {
        source,
        target: target.clone(),
        kind: EdgeKind::DependsOn,
        confidence: 1.0,
        direction: None,
        operation: Some(declaration.kind.as_str().to_string()),
        condition: None,
        async_boundary: None,
        provenance: vec![EdgeProvenance {
            file: declaration.manifest_path.clone(),
            span: manifest_span(root, &declaration.manifest_path),
            symbol_id: target,
        }],
        repo: None,
    }
}

fn package_node_id(manifest_path: &Path) -> String {
    format!("{}::cargo_package", normalize_path(manifest_path))
}

fn dependency_node_id(
    manifest_path: &Path,
    kind: CargoDependencyKind,
    dependency_name: &str,
) -> String {
    format!(
        "{}::cargo_dependency::{}::{}",
        normalize_path(manifest_path),
        kind.as_str(),
        dependency_name
    )
}

fn relative_manifest_path(root: &Path, manifest_path: &Path) -> PathBuf {
    manifest_path
        .strip_prefix(root)
        .unwrap_or(manifest_path)
        .to_path_buf()
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn manifest_span(root: &Path, manifest_path: &Path) -> Span {
    let line_count = std::fs::read_to_string(root.join(manifest_path))
        .ok()
        .map(|content| content.lines().count().max(1))
        .unwrap_or(1);
    Span {
        start: [1, 0],
        end: [line_count, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::EdgeKind;
    use std::fs;

    #[test]
    fn extracts_workspace_and_package_dependency_declarations() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[workspace]
members = ["crates/*"]

[workspace.dependencies]
mentra = { version = "0.7", git = "https://example.invalid/mentra.git" }
serde = "1"
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("crates/app/Cargo.toml"),
            r#"
[package]
name = "app"

[dependencies]
mentra = { workspace = true }
local_tools = { path = "../local-tools" }

[dev-dependencies]
insta = "1"

[build-dependencies]
cc = { version = "1", package = "cc" }
"#,
        )
        .unwrap();

        let result = extract_dependency_graph(dir.path());

        assert!(result.nodes.iter().any(|node| {
            node.id == "Cargo.toml::cargo_dependency::workspace::mentra"
                && node
                    .metadata
                    .get("cargo.dependency.version_req")
                    .map(String::as_str)
                    == Some("0.7")
                && node
                    .metadata
                    .get("cargo.dependency.source")
                    .map(String::as_str)
                    == Some("git")
        }));
        assert!(result.nodes.iter().any(|node| {
            node.id == "crates/app/Cargo.toml::cargo_dependency::normal::mentra"
                && node
                    .metadata
                    .get("cargo.dependency.version_req")
                    .map(String::as_str)
                    == Some("0.7")
                && node
                    .metadata
                    .get("cargo.dependency.inherited_workspace")
                    .map(String::as_str)
                    == Some("true")
        }));
        assert!(result.nodes.iter().any(|node| {
            node.id == "crates/app/Cargo.toml::cargo_dependency::normal::local_tools"
                && node
                    .metadata
                    .get("cargo.dependency.source")
                    .map(String::as_str)
                    == Some("path")
        }));
        assert!(result.nodes.iter().any(|node| {
            node.id == "crates/app/Cargo.toml::cargo_dependency::build::cc"
                && node
                    .metadata
                    .get("cargo.dependency.package")
                    .map(String::as_str)
                    == Some("cc")
        }));
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::DependsOn)
                .count(),
            6
        );
    }

    #[test]
    fn extracts_package_node_for_manifest_without_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "empty_app"
"#,
        )
        .unwrap();

        let result = extract_dependency_graph(dir.path());

        assert_eq!(result.edges.len(), 0);
        assert!(result.nodes.iter().any(|node| {
            node.id == "Cargo.toml::cargo_package"
                && node.name == "empty_app"
                && node.metadata.get("cargo.node").map(String::as_str) == Some("package")
        }));
    }
}
