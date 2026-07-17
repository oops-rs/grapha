use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use grapha_core::ExtractionResult;
use grapha_core::graph::{Edge, EdgeKind, EdgeProvenance, Node, NodeKind, Span, Visibility};
use toml::{Table, Value};
use toml_edit::{ImDocument, Item, Key, TableLike};

use crate::extractor::{DependencyDeclaration, ManifestDependencyExtractor, ManifestPath};

const DEP_METADATA_PREFIX: &str = "cargo.dependency.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CargoDependencyKind {
    Normal,
    Dev,
    Build,
    Workspace,
}

impl CargoDependencyKind {
    #[must_use]
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
    /// The declaration's exact source line/column within its own manifest
    /// (ADR-0069 OQ1) — the `key = value` (or `[dependencies.name]` table
    /// header through its last field) byte range, converted to 1-indexed
    /// line/col via [`LineIndex`]. An inherited (`{ workspace = true }`)
    /// declaration's span is where *that* line appears in the consuming
    /// manifest, not the `[workspace.dependencies]` line it borrows fields
    /// from — those are two distinct, separately-spanned nodes.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDependency {
    name: String,
    package_name: Option<String>,
    version_req: Option<String>,
    source: String,
    source_detail: Option<String>,
    workspace: bool,
    span: Span,
}

/// # Errors
///
/// Returns an error when `content` is not valid TOML.
pub fn parse_manifest_table(content: &str) -> Result<Table, toml::de::Error> {
    content.parse::<Table>()
}

/// # Errors
///
/// Returns an error when `path` cannot be read or is not valid TOML.
pub fn read_manifest_table(path: &Path) -> anyhow::Result<Table> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_manifest_table(&content)?)
}

/// Parse a manifest with `toml_edit`'s span-preserving `ImDocument` (ADR-0069
/// OQ1), kept deliberately separate from [`read_manifest_table`]: module
/// discovery (`rust_plugin.rs`/`module.rs`, via `read_manifest_table` /
/// `package_name_from_table` / `workspace_member_paths_from_table`) never
/// needed declaration-level spans, so it keeps its existing `toml::Table`
/// parse untouched. Only dependency-declaration extraction, which does need
/// spans, reads the manifest a second time through this function.
fn read_manifest_document(path: &Path) -> anyhow::Result<ImDocument<String>> {
    let content = std::fs::read_to_string(path)?;
    Ok(ImDocument::parse(content)?)
}

/// Maps a byte offset within one manifest's raw text to a 1-indexed
/// `[line, col]` pair (col 0-indexed), matching the whole-file fallback
/// span's existing `[1, 0]` / `[line_count, 0]` shape. Newline offsets are
/// collected once per manifest so converting each declaration's span is a
/// binary search rather than a fresh linear scan.
struct LineIndex {
    newline_offsets: Vec<usize>,
}

impl LineIndex {
    fn new(content: &str) -> Self {
        let newline_offsets = content
            .bytes()
            .enumerate()
            .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset))
            .collect();
        Self { newline_offsets }
    }

    fn line_col(&self, offset: usize) -> [usize; 2] {
        // Number of newlines strictly before `offset` = how many lines precede it.
        let preceding_newlines = self.newline_offsets.partition_point(|&nl| nl < offset);
        let line = preceding_newlines + 1;
        let col = if preceding_newlines == 0 {
            offset
        } else {
            offset - self.newline_offsets[preceding_newlines - 1] - 1
        };
        [line, col]
    }

    fn span(&self, range: std::ops::Range<usize>) -> Span {
        Span {
            start: self.line_col(range.start),
            end: self.line_col(range.end),
        }
    }
}

/// The declaration's exact span: the union of its key's and value's byte
/// ranges (so `mentra = { workspace = true }` spans from `mentra` through the
/// closing `}`), converted via `line_index`. Falls back to `fallback` in the
/// defensive case where `toml_edit` cannot report a span for either half —
/// this should not happen for a document parsed via `ImDocument::parse`, but
/// a fallback keeps this function total rather than panicking.
fn declaration_span(key: &Key, item: &Item, line_index: &LineIndex, fallback: &Span) -> Span {
    let key_range = key.span();
    let item_range = item.span();
    let range = match (key_range, item_range) {
        (Some(k), Some(v)) => Some(k.start.min(v.start)..k.end.max(v.end)),
        (Some(k), None) => Some(k),
        (None, Some(v)) => Some(v),
        (None, None) => None,
    };
    range.map_or_else(|| fallback.clone(), |range| line_index.span(range))
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

#[must_use]
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

#[must_use]
pub fn dependency_metadata_key(suffix: &str) -> String {
    format!("{DEP_METADATA_PREFIX}{suffix}")
}

#[must_use]
pub fn extract_dependency_graph(root: &Path) -> ExtractionResult {
    // Discovery is routed through the `ManifestDependencyExtractor` seam
    // (ADR-0069 Decision 2) even though Cargo is still the only ecosystem —
    // `CargoDependencyExtractor::discover` is exactly
    // `discover_cargo_manifest_paths`, just reached through the trait an
    // npm/pyproject implementation will also satisfy.
    let extractor = CargoDependencyExtractor;
    let manifest_paths: Vec<PathBuf> = extractor
        .discover(root)
        .into_iter()
        .map(|manifest| manifest.0)
        .collect();
    if manifest_paths.is_empty() {
        return ExtractionResult::new();
    }

    let root_manifest_path = root.join("Cargo.toml");
    let workspace_dependencies = read_manifest_document(&root_manifest_path)
        .ok()
        .map(|document| {
            let line_index = LineIndex::new(document.raw());
            let fallback = manifest_span(root, Path::new("Cargo.toml"));
            parse_workspace_dependencies(&document, &line_index, &fallback)
        })
        .unwrap_or_default();

    let mut packages = BTreeMap::new();
    let mut declarations = Vec::new();
    for manifest_path in manifest_paths {
        // Two parses of the same file: `table` (plain `toml`) drives package
        // naming exactly as before; `document` (`toml_edit`) is the new,
        // span-preserving read used only for declaration spans. Keeping them
        // separate means module discovery's existing, tested parse path is
        // untouched by this change.
        let Ok(table) = read_manifest_table(&manifest_path) else {
            continue;
        };
        let manifest_rel = relative_manifest_path(root, &manifest_path);
        let package = package_name_from_table(
            &table,
            manifest_path.parent().unwrap_or_else(|| Path::new("")),
        );
        packages.insert(manifest_rel.clone(), package.clone());

        let Ok(document) = read_manifest_document(&manifest_path) else {
            continue;
        };
        let line_index = LineIndex::new(document.raw());
        let fallback = manifest_span(root, &manifest_rel);
        let is_workspace_root = manifest_path == root_manifest_path;
        declarations.extend(declarations_from_table(
            &manifest_rel,
            &package,
            &document,
            &line_index,
            &fallback,
            is_workspace_root,
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

fn parse_workspace_dependencies(
    document: &ImDocument<String>,
    line_index: &LineIndex,
    fallback_span: &Span,
) -> BTreeMap<String, ParsedDependency> {
    document
        .get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Item::as_table_like)
        .map(|table| parse_dependency_table(table, line_index, fallback_span))
        .unwrap_or_default()
}

/// Builds this manifest's dependency declarations, each carrying its own
/// exact source span (ADR-0069 OQ1). `is_workspace_root` gates whether
/// `[workspace.dependencies]` entries (already parsed once, from the
/// workspace root, by the caller) are also emitted as `Workspace`-kind
/// declarations for *this* manifest — true only when this manifest **is**
/// the workspace root, mirroring the original `manifest_path ==
/// root.join("Cargo.toml")` check.
fn declarations_from_table(
    manifest_rel: &Path,
    package: &str,
    document: &ImDocument<String>,
    line_index: &LineIndex,
    fallback_span: &Span,
    is_workspace_root: bool,
    workspace_dependencies: &BTreeMap<String, ParsedDependency>,
) -> Vec<CargoDependencyDeclaration> {
    let mut declarations = Vec::new();

    if is_workspace_root {
        declarations.extend(workspace_dependencies.values().map(|dependency| {
            declaration_from_dependency(
                manifest_rel.to_path_buf(),
                package.to_string(),
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
        let Some(dependencies) = document.get(section).and_then(Item::as_table_like) else {
            continue;
        };
        for dependency in
            parse_dependency_table(dependencies, line_index, fallback_span).into_values()
        {
            // `dependency`'s span was captured against *this* manifest above,
            // regardless of whether it inherits fields from the workspace
            // below — inheritance merges values, never location.
            let (dependency, inherited_workspace) = if dependency.workspace {
                let merged = workspace_dependencies.get(&dependency.name).map_or_else(
                    || dependency.clone(),
                    |workspace| merge_workspace_dependency(dependency.clone(), workspace),
                );
                (merged, true)
            } else {
                (dependency, false)
            };
            declarations.push(declaration_from_dependency(
                manifest_rel.to_path_buf(),
                package.to_string(),
                dependency,
                kind,
                inherited_workspace,
            ));
        }
    }

    declarations
}

/// Parses every entry of a `[dependencies]`-shaped table (or
/// `[workspace.dependencies]`), regardless of whether TOML wrote it as an
/// inline table (`name = { version = "1" }`) or a full sub-table
/// (`[dependencies.name]`) — [`TableLike`] unifies both, matching the
/// original `toml::Value::Table` match arm's behavior (which made no such
/// distinction, because plain `toml` discards the inline/full-table
/// formatting difference by the time it reaches `Value`).
fn parse_dependency_table(
    table: &dyn TableLike,
    line_index: &LineIndex,
    fallback_span: &Span,
) -> BTreeMap<String, ParsedDependency> {
    table
        .iter()
        .map(|(name, _item)| {
            let (key, item) = table
                .get_key_value(name)
                .expect("key yielded by iter() exists in the same table");
            let span = declaration_span(key, item, line_index, fallback_span);
            (name.to_string(), parse_dependency(name, item, span))
        })
        .collect()
}

fn parse_dependency(name: &str, item: &Item, span: Span) -> ParsedDependency {
    if let Some(table) = item.as_table_like() {
        let path = table.get("path").and_then(Item::as_str).map(str::to_string);
        let git = table.get("git").and_then(Item::as_str).map(str::to_string);
        let registry = table
            .get("registry")
            .and_then(Item::as_str)
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
                .and_then(Item::as_str)
                .map(str::to_string),
            version_req: table
                .get("version")
                .and_then(Item::as_str)
                .map(str::to_string),
            source,
            source_detail,
            workspace: table
                .get("workspace")
                .and_then(Item::as_bool)
                .unwrap_or(false),
            span,
        }
    } else {
        // A bare string version (`name = "1"`) or any other scalar
        // (`Item::as_str` is `None` for non-string scalars, matching the
        // original catch-all arm's `version_req: None`).
        ParsedDependency {
            name: name.to_string(),
            package_name: None,
            version_req: item.as_str().map(str::to_string),
            source: "registry".to_string(),
            source_detail: None,
            workspace: false,
            span,
        }
    }
}

fn merge_workspace_dependency(
    mut dependency: ParsedDependency,
    workspace: &ParsedDependency,
) -> ParsedDependency {
    if dependency.package_name.is_none() {
        dependency.package_name.clone_from(&workspace.package_name);
    }
    if dependency.version_req.is_none() {
        dependency.version_req.clone_from(&workspace.version_req);
    }
    if dependency.source == "registry" && dependency.source_detail.is_none() {
        dependency.source.clone_from(&workspace.source);
        dependency
            .source_detail
            .clone_from(&workspace.source_detail);
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
        span: dependency.span,
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
            .push(dependency_node(&dependency_id, &declaration));
        result
            .edges
            .push(dependency_edge(package_id, dependency_id, &declaration));
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

fn dependency_node(id: &str, declaration: &CargoDependencyDeclaration) -> Node {
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
        span: declaration.span.clone(),
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
            span: declaration.span.clone(),
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
        .map_or(1, |content| content.lines().count().max(1));
    Span {
        start: [1, 0],
        end: [line_count, 0],
    }
}

/// Cargo's [`ManifestDependencyExtractor`] implementation (ADR-0069 Decision
/// 2). `discover` wraps [`discover_cargo_manifest_paths`] unchanged.
/// `declarations` reads one manifest's `[dependencies]` /
/// `[dev-dependencies]` / `[build-dependencies]` entries with real spans, but
/// deliberately does **not** resolve `[workspace.dependencies]` inheritance —
/// that requires jointly reading the workspace root alongside the member
/// manifest, which is Cargo-specific glue that belongs in
/// [`extract_dependency_graph`] (the full graph-building pipeline), not in
/// this ecosystem-neutral seam. A `{ workspace = true }` entry surfaces here
/// with whatever fields it wrote itself (often just `workspace = true`, so
/// `version_req` is `None`); `extract_dependency_graph` is what promotes it to
/// a fully-merged declaration.
pub struct CargoDependencyExtractor;

impl ManifestDependencyExtractor for CargoDependencyExtractor {
    fn discover(&self, root: &Path) -> Vec<ManifestPath> {
        discover_cargo_manifest_paths(root)
            .into_iter()
            .map(ManifestPath)
            .collect()
    }

    fn declarations(&self, manifest: &ManifestPath) -> Vec<DependencyDeclaration> {
        let manifest_path = manifest.as_path();
        let Ok(document) = read_manifest_document(manifest_path) else {
            return Vec::new();
        };
        let line_index = LineIndex::new(document.raw());
        let fallback = Span {
            start: [1, 0],
            end: [line_index.newline_offsets.len().max(1), 0],
        };

        [
            ("dependencies", "normal"),
            ("dev-dependencies", "dev"),
            ("build-dependencies", "build"),
        ]
        .into_iter()
        .filter_map(|(section, kind)| {
            let table = document.get(section).and_then(Item::as_table_like)?;
            Some(
                parse_dependency_table(table, &line_index, &fallback)
                    .into_values()
                    .map(move |dependency| DependencyDeclaration {
                        name: dependency.name,
                        package_name: dependency.package_name,
                        version_req: dependency.version_req,
                        source: dependency.source,
                        kind: kind.to_string(),
                        manifest_path: manifest_path.to_path_buf(),
                    }),
            )
        })
        .flatten()
        .collect()
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

    /// ADR-0069 OQ1: every dependency declaration's node span points at its
    /// own real source line, not the whole-manifest `[1, 0]..[N, 0]`
    /// fallback `manifest_span` still uses for package nodes.
    #[test]
    fn dependency_declarations_carry_exact_line_spans() {
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
        let span_of = |id: &str| -> Span {
            result
                .nodes
                .iter()
                .find(|node| node.id == id)
                .unwrap_or_else(|| panic!("missing node {id}"))
                .span
                .clone()
        };

        // Root `[workspace.dependencies]` declarations: lines 6 and 7 of the
        // root Cargo.toml (line 1 is the leading blank line before
        // `[workspace]`).
        assert_eq!(
            span_of("Cargo.toml::cargo_dependency::workspace::mentra").start,
            [6, 0]
        );
        assert_eq!(
            span_of("Cargo.toml::cargo_dependency::workspace::serde").start,
            [7, 0]
        );

        // The consuming manifest's own declarations, each on its own real
        // line within `crates/app/Cargo.toml` — including the
        // `{ workspace = true }` entry, whose span is where *it* is written,
        // not where the root's `[workspace.dependencies]` entry is.
        assert_eq!(
            span_of("crates/app/Cargo.toml::cargo_dependency::normal::mentra").start,
            [6, 0]
        );
        assert_eq!(
            span_of("crates/app/Cargo.toml::cargo_dependency::normal::local_tools").start,
            [7, 0]
        );
        assert_eq!(
            span_of("crates/app/Cargo.toml::cargo_dependency::dev::insta").start,
            [10, 0]
        );
        assert_eq!(
            span_of("crates/app/Cargo.toml::cargo_dependency::build::cc").start,
            [13, 0]
        );

        // Sanity: distinct declarations in the same manifest get distinct
        // spans, so this isn't accidentally another shared fallback.
        let mentra_span = span_of("crates/app/Cargo.toml::cargo_dependency::normal::mentra");
        let local_tools_span =
            span_of("crates/app/Cargo.toml::cargo_dependency::normal::local_tools");
        assert_ne!(mentra_span.start, local_tools_span.start);

        // The edge provenance span (what `code_dependents` will eventually
        // cite through `dependency_edge`) matches the node span exactly.
        let edge = result
            .edges
            .iter()
            .find(|edge| edge.target == "crates/app/Cargo.toml::cargo_dependency::build::cc")
            .expect("cc DependsOn edge");
        assert_eq!(edge.provenance[0].span.start, [13, 0]);
    }

    #[test]
    fn line_index_converts_byte_offsets_to_one_indexed_line_col() {
        let content = "a = 1\nb = 2\nc = 3\n";
        let index = LineIndex::new(content);
        assert_eq!(index.line_col(0), [1, 0]);
        assert_eq!(index.line_col(6), [2, 0]);
        assert_eq!(index.line_col(12), [3, 0]);
    }

    /// ADR-0069 Decision 2: `CargoDependencyExtractor` satisfies
    /// `ManifestDependencyExtractor` directly, producing the ecosystem-neutral
    /// `DependencyDeclaration` shape (no workspace-inheritance resolution —
    /// that stays in `extract_dependency_graph`, see the trait's doc comment).
    #[test]
    fn cargo_dependency_extractor_satisfies_the_seam() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "solo"

[dependencies]
serde = { version = "1", features = ["derive"] }
"#,
        )
        .unwrap();

        let extractor = CargoDependencyExtractor;
        let manifests = extractor.discover(dir.path());
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].as_path(), dir.path().join("Cargo.toml"));

        let declarations = extractor.declarations(&manifests[0]);
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].name, "serde");
        assert_eq!(declarations[0].version_req.as_deref(), Some("1"));
        assert_eq!(declarations[0].source, "registry");
        assert_eq!(declarations[0].kind, "normal");
        assert_eq!(declarations[0].manifest_path, dir.path().join("Cargo.toml"));
    }
}
