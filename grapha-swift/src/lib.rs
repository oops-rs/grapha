mod binary;
mod bridge;
mod classifier;
mod graph_pass;
mod indexstore;
mod module_discovery;
mod swiftsyntax;
mod treesitter;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Instant;

/// Thread-summed timing counters (nanoseconds) for extraction phases.
/// Callers can read these after extraction to report breakdowns.
pub static TIMING_INDEXSTORE_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_PARSE_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_ENRICH_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_DOC_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_SWIFTUI_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_L10N_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_ASSET_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_SWIFTSYNTAX_NS: AtomicU64 = AtomicU64::new(0);
pub static TIMING_TS_FALLBACK_NS: AtomicU64 = AtomicU64::new(0);

pub use treesitter::SwiftExtractor;

use grapha_core::{
    Classifier, ExtractionResult, FileContext, GraphPass, LanguageExtractor, LanguagePlugin,
    LanguageRegistry, ModuleMap, ProjectContext, SemanticAnnotation, SemanticDocument,
};

static INDEX_STORE_PATHS: LazyLock<RwLock<HashMap<PathBuf, Option<PathBuf>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub struct SwiftPlugin;

impl LanguagePlugin for SwiftPlugin {
    fn id(&self) -> &'static str {
        "swift"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["swift"]
    }

    fn prepare_project(&self, context: &ProjectContext) -> anyhow::Result<()> {
        if context.index_store_enabled {
            prepare_project_index_store(&context.project_root);
        } else {
            clear_index_store_path(&context.project_root);
        }
        Ok(())
    }

    fn finish_project(&self, _context: &ProjectContext) -> anyhow::Result<()> {
        indexstore::release_store_handles();
        Ok(())
    }

    fn discover_modules(&self, context: &ProjectContext) -> anyhow::Result<ModuleMap> {
        Ok(module_discovery::discover_swift_modules(
            &context.project_root,
        ))
    }

    fn extract(&self, source: &[u8], context: &FileContext) -> anyhow::Result<ExtractionResult> {
        extract_swift(
            source,
            &context.relative_path,
            None,
            Some(&context.project_root),
            context.index_store_enabled,
        )
    }

    fn stamp_module(
        &self,
        result: ExtractionResult,
        module_name: Option<&str>,
    ) -> ExtractionResult {
        stamp_swift_module(result, module_name)
    }

    fn extract_semantics(
        &self,
        source: &[u8],
        context: &FileContext,
    ) -> anyhow::Result<SemanticDocument> {
        let mut document = SemanticDocument::from_extraction_result(self.extract(source, context)?);

        document.annotate_call_relations(|relation, _source| {
            classifier::terminal_effect_for_target(relation.target.as_raw())
                .or_else(|| graph_pass::terminal_effect_for_usr_target(relation.target.as_raw()))
        });

        let mut entry_points = Vec::new();
        for relation in &document.relations {
            if relation.kind == grapha_core::graph::EdgeKind::Implements
                && graph_pass::is_entry_point_target(relation.target.as_raw())
            {
                entry_points.push(relation.source.clone());
            }
        }
        for symbol in &mut document.symbols {
            if entry_points.iter().any(|symbol_id| symbol_id == &symbol.id)
                && !symbol.annotations.contains(&SemanticAnnotation::EntryPoint)
            {
                symbol.annotations.push(SemanticAnnotation::EntryPoint);
            }
        }

        Ok(document)
    }

    fn classifiers(&self) -> Vec<Box<dyn Classifier>> {
        Vec::new()
    }

    fn graph_passes(&self) -> Vec<Box<dyn GraphPass>> {
        Vec::new()
    }
}

pub fn register_builtin(registry: &mut LanguageRegistry) -> anyhow::Result<()> {
    registry.register(SwiftPlugin)
}

/// Auto-discover the Xcode index store for a project.
/// Walks up from the given path looking for .xcodeproj/.xcworkspace,
/// then matches the Xcode project name against DerivedData folders.
fn discover_index_store(start_path: &Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let derived_data = Path::new(&home).join("Library/Developer/Xcode/DerivedData");
    if !derived_data.is_dir() {
        return None;
    }

    // Collect candidate project names by walking up and looking for .xcodeproj
    let mut candidates = Vec::new();
    let mut dir = if start_path.is_file() {
        start_path.parent().map(Path::to_path_buf)
    } else {
        Some(start_path.to_path_buf())
    };

    while let Some(d) = dir {
        // Check for .xcodeproj or .xcworkspace
        if let Ok(entries) = std::fs::read_dir(&d) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(".xcodeproj") || name_str.ends_with(".xcworkspace") {
                    let project_name = name_str
                        .trim_end_matches(".xcodeproj")
                        .trim_end_matches(".xcworkspace")
                        .to_string();
                    candidates.push(project_name);
                }
            }
        }
        // Also add the directory name itself as a candidate
        if let Some(dir_name) = d.file_name().and_then(|n| n.to_str()) {
            candidates.push(dir_name.replace(['-', '_', '.'], "").to_lowercase());
        }
        dir = d.parent().map(Path::to_path_buf);
        // Don't walk above home directory
        if dir.as_deref() == Some(Path::new(&home)) {
            break;
        }
    }

    // Match candidates against DerivedData folders
    for entry in std::fs::read_dir(&derived_data).ok()? {
        let entry = entry.ok()?;
        let dd_name = entry.file_name();
        let dd_str = dd_name.to_string_lossy();
        // DerivedData folder format: "<ProjectName>-<hash>"
        let dd_project = dd_str.split('-').next().unwrap_or(&dd_str).to_lowercase();

        for candidate in &candidates {
            let candidate_lower = candidate.to_lowercase();
            if dd_project == candidate_lower {
                let store = entry.path().join("Index.noindex/DataStore");
                if store.is_dir() {
                    return Some(store);
                }
            }
        }
    }
    None
}

/// Pre-discover the index store path. Call before starting extraction
/// to ensure the discovery log appears before the progress bar.
pub fn init_index_store(project_root: &Path) {
    let _ = init_index_store_with(&INDEX_STORE_PATHS, project_root, discover_index_store);
}

fn prepare_project_index_store(project_root: &Path) {
    prepare_project_with(&INDEX_STORE_PATHS, project_root, discover_index_store);
    if let Some(store_path) = index_store_path(project_root) {
        indexstore::warmup_indexstore(&store_path);
    }
}

fn prepare_project_with<F>(
    cache: &RwLock<HashMap<PathBuf, Option<PathBuf>>>,
    project_root: &Path,
    discover: F,
) where
    F: FnMut(&Path) -> Option<PathBuf>,
{
    let _ = refresh_index_store_with(cache, project_root, discover);
}

fn set_index_store_path_in(
    cache: &RwLock<HashMap<PathBuf, Option<PathBuf>>>,
    project_root: &Path,
    store: Option<PathBuf>,
) {
    cache
        .write()
        .expect("index-store cache poisoned")
        .insert(project_cache_key(project_root), store);
}

/// Force index-store rediscovery for a project, including after a cached miss.
pub fn refresh_index_store(project_root: &Path) -> Option<PathBuf> {
    refresh_index_store_with(&INDEX_STORE_PATHS, project_root, discover_index_store)
}

fn init_index_store_with<F>(
    cache: &RwLock<HashMap<PathBuf, Option<PathBuf>>>,
    project_root: &Path,
    discover: F,
) -> Option<PathBuf>
where
    F: FnMut(&Path) -> Option<PathBuf>,
{
    let key = project_cache_key(project_root);
    if let Some(existing) = cached_index_store_path_in(cache, &key) {
        return existing;
    }

    let discovered = discover_index_store_with(&key, discover);

    cache
        .write()
        .expect("index-store cache poisoned")
        .entry(key)
        .or_insert_with(|| discovered.clone());

    discovered
}

fn refresh_index_store_with<F>(
    cache: &RwLock<HashMap<PathBuf, Option<PathBuf>>>,
    project_root: &Path,
    discover: F,
) -> Option<PathBuf>
where
    F: FnMut(&Path) -> Option<PathBuf>,
{
    cache
        .write()
        .expect("index-store cache poisoned")
        .remove(&project_cache_key(project_root));
    init_index_store_with(cache, project_root, discover)
}

fn project_cache_key(project_root: &Path) -> PathBuf {
    if project_root.extension().is_some_and(|ext| ext == "swift") {
        project_root.parent().unwrap_or(project_root).to_path_buf()
    } else {
        project_root.to_path_buf()
    }
}

pub fn index_store_path(project_root: &Path) -> Option<PathBuf> {
    index_store_path_in(&INDEX_STORE_PATHS, project_root)
}

/// Seed or clear the cached index-store path for a project.
///
/// This is primarily useful for tests and for explicit cache invalidation when
/// a caller knows index-store should not be used for a project.
pub fn set_index_store_path(project_root: &Path, store: Option<PathBuf>) {
    set_index_store_path_in(&INDEX_STORE_PATHS, project_root, store);
}

/// Clear any cached index-store path for a project.
pub fn clear_index_store_path(project_root: &Path) {
    set_index_store_path(project_root, None);
}

fn index_store_path_in(
    cache: &RwLock<HashMap<PathBuf, Option<PathBuf>>>,
    project_root: &Path,
) -> Option<PathBuf> {
    cached_index_store_path_in(cache, project_root).flatten()
}

fn cached_index_store_path_in(
    cache: &RwLock<HashMap<PathBuf, Option<PathBuf>>>,
    project_root: &Path,
) -> Option<Option<PathBuf>> {
    cache
        .read()
        .expect("index-store cache poisoned")
        .get(&project_cache_key(project_root))
        .cloned()
}

fn discover_index_store_with<F>(start_path: &Path, mut discover: F) -> Option<PathBuf>
where
    F: FnMut(&Path) -> Option<PathBuf>,
{
    if let Some(store) = discover(start_path) {
        return Some(store);
    }

    let mut dir = start_path.parent().map(Path::to_path_buf);
    while let Some(candidate) = dir {
        if let Some(store) = discover(&candidate) {
            return Some(store);
        }
        dir = candidate.parent().map(Path::to_path_buf);
    }

    None
}

fn stamp_swift_module(result: ExtractionResult, module_name: Option<&str>) -> ExtractionResult {
    let Some(module_name) = module_name else {
        return result;
    };

    let manifest_id_remap: std::collections::HashMap<String, String> = result
        .nodes
        .iter()
        .filter(|node| {
            node.file.file_name().and_then(|name| name.to_str()) == Some("Package.swift")
        })
        .map(|node| {
            (
                node.id.clone(),
                format!("{}@@module:{}", node.id, module_name),
            )
        })
        .collect();

    let nodes = result
        .nodes
        .into_iter()
        .map(|mut node| {
            node.module = Some(module_name.to_string());
            if let Some(remapped_id) = manifest_id_remap.get(&node.id) {
                node.id = remapped_id.clone();
            }
            node
        })
        .collect();

    let edges = result
        .edges
        .into_iter()
        .map(|mut edge| {
            if let Some(remapped_id) = manifest_id_remap.get(&edge.source) {
                edge.source = remapped_id.clone();
            }
            if let Some(remapped_id) = manifest_id_remap.get(&edge.target) {
                edge.target = remapped_id.clone();
            }
            for provenance in &mut edge.provenance {
                if let Some(remapped_id) = manifest_id_remap.get(&provenance.symbol_id) {
                    provenance.symbol_id = remapped_id.clone();
                }
            }
            edge
        })
        .collect();

    ExtractionResult {
        nodes,
        edges,
        imports: result.imports,
    }
}

fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Fast byte-level check for SwiftUI markers to skip expensive enrichment.
fn source_contains_swiftui_markers(source: &[u8]) -> bool {
    bytes_contains(source, b": View")
        || bytes_contains(source, b": App ")
        || bytes_contains(source, b": App{")
        || bytes_contains(source, b"some View")
        || bytes_contains(source, b"any View")
        || bytes_contains(source, b"some SwiftUI.View")
        || bytes_contains(source, b"any SwiftUI.View")
        || bytes_contains(source, b"@ViewBuilder")
}

/// Fast byte-level check for localization markers to skip l10n enrichment.
fn source_contains_l10n_markers(source: &[u8]) -> bool {
    bytes_contains(source, b"L10n")
        || bytes_contains(source, b"NSLocalizedString")
        || bytes_contains(source, b"LocalizedStringKey")
        || bytes_contains(source, b"Text(\"")
        || bytes_contains(source, b"Text(.")
        || bytes_contains(source, b"Text(i18n:")
        || bytes_contains(source, b"String(i18n:")
        || bytes_contains(source, b"i18n:")
        || bytes_contains(source, b".translation")
        || bytes_contains(source, b"Localizable")
}

/// Fast byte-level check for image asset markers.
fn source_contains_asset_markers(source: &[u8]) -> bool {
    treesitter::source_contains_image_asset_markers(source)
}

/// Extract Swift source code with waterfall strategy:
/// 1. Xcode index store (confidence 1.0)
/// 2. SwiftSyntax bridge (confidence 0.9)
/// 3. tree-sitter-swift fallback (confidence 0.6-0.8)
pub fn extract_swift(
    source: &[u8],
    file_path: &Path,
    explicit_index_store_path: Option<&Path>,
    project_root: Option<&Path>,
    index_store_enabled: bool,
) -> anyhow::Result<ExtractionResult> {
    extract_swift_with_init(
        source,
        file_path,
        explicit_index_store_path,
        project_root,
        index_store_enabled,
        init_index_store,
    )
}

fn extract_swift_with_init<F>(
    source: &[u8],
    file_path: &Path,
    explicit_index_store_path: Option<&Path>,
    project_root: Option<&Path>,
    index_store_enabled: bool,
    mut init_index_store_fn: F,
) -> anyhow::Result<ExtractionResult>
where
    F: FnMut(&Path),
{
    if index_store_enabled && let Some(root) = project_root {
        init_index_store_fn(root);
    }

    let effective_store = if index_store_enabled {
        explicit_index_store_path
            .map(Path::to_path_buf)
            .or_else(|| project_root.and_then(index_store_path))
    } else {
        None
    };

    if let Some(store_path) = effective_store.as_deref() {
        // Index store needs absolute file path for matching
        let abs_file = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else if let Some(root) = project_root {
            if root.is_file() {
                // Single file analysis: root IS the absolute file path
                root.to_path_buf()
            } else {
                root.join(file_path)
            }
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(file_path))
                .unwrap_or_else(|_| file_path.to_path_buf())
        };
        // Use abs_file directly — canonicalize is expensive (syscall per file)
        // and only matters if there are symlinks, which is rare for source files.
        let canonical_file = abs_file;
        let t_is = Instant::now();
        let is_result = indexstore::extract_from_indexstore(&canonical_file, store_path);
        TIMING_INDEXSTORE_NS.fetch_add(t_is.elapsed().as_nanos() as u64, Ordering::Relaxed);

        if let Some(mut result) = is_result {
            restamp_indexstore_file_path(&mut result, file_path);
            // Index store doesn't provide doc comments — enrich via tree-sitter.
            // Parse once, share tree across all enrichment passes.
            // Check which enrichment passes are needed before parsing
            let has_swiftui = source_contains_swiftui_markers(source);
            let has_l10n = has_swiftui || source_contains_l10n_markers(source);
            let has_assets = source_contains_asset_markers(source);
            let needs_doc = result.nodes.iter().any(|n| n.doc_comment.is_none());
            let needs_compat = !result
                .nodes
                .iter()
                .any(|node| node.kind == grapha_core::graph::NodeKind::File);
            let needs_parse = needs_compat || needs_doc || has_swiftui || has_l10n || has_assets;

            if needs_parse {
                let t_parse = Instant::now();
                let tree_result = treesitter::parse_swift(source);
                TIMING_TS_PARSE_NS
                    .fetch_add(t_parse.elapsed().as_nanos() as u64, Ordering::Relaxed);

                if let Ok(tree) = tree_result {
                    if needs_compat {
                        let _ = treesitter::enrich_codegraph_compat_with_tree(
                            source,
                            file_path,
                            &tree,
                            &mut result,
                        );
                    }

                    if needs_doc {
                        let t_doc = Instant::now();
                        let _ =
                            treesitter::enrich_doc_comments_with_tree(source, &tree, &mut result);
                        TIMING_TS_DOC_NS
                            .fetch_add(t_doc.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }

                    if has_swiftui {
                        let t_swiftui = Instant::now();
                        let _ = treesitter::enrich_swiftui_structure_with_tree(
                            source,
                            file_path,
                            &tree,
                            &mut result,
                        );
                        TIMING_TS_SWIFTUI_NS
                            .fetch_add(t_swiftui.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }

                    if has_l10n {
                        let t_l10n = Instant::now();
                        let _ = treesitter::enrich_localization_metadata_with_tree(
                            source,
                            file_path,
                            &tree,
                            &mut result,
                        );
                        TIMING_TS_L10N_NS
                            .fetch_add(t_l10n.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }

                    if has_assets {
                        let t_asset = Instant::now();
                        let _ = treesitter::enrich_asset_references_with_tree(
                            source,
                            file_path,
                            &tree,
                            &mut result,
                        );
                        TIMING_TS_ASSET_NS
                            .fetch_add(t_asset.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }
                }
            }
            return Ok(result);
        }
    }

    let t_ss = Instant::now();
    if let Some(mut result) = swiftsyntax::extract_with_swiftsyntax(source, file_path) {
        TIMING_SWIFTSYNTAX_NS.fetch_add(t_ss.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let t_parse = Instant::now();
        let tree_result = treesitter::parse_swift(source);
        TIMING_TS_PARSE_NS.fetch_add(t_parse.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if let Ok(tree) = tree_result {
            let t_enrich = Instant::now();
            let _ = treesitter::enrich_codegraph_compat_with_tree(
                source,
                file_path,
                &tree,
                &mut result,
            );
            let _ = treesitter::enrich_doc_comments_with_tree(source, &tree, &mut result);
            let _ = treesitter::enrich_swiftui_structure_with_tree(
                source,
                file_path,
                &tree,
                &mut result,
            );
            let _ = treesitter::enrich_localization_metadata_with_tree(
                source,
                file_path,
                &tree,
                &mut result,
            );
            let _ = treesitter::enrich_asset_references_with_tree(
                source,
                file_path,
                &tree,
                &mut result,
            );
            TIMING_TS_ENRICH_NS.fetch_add(t_enrich.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        return Ok(result);
    }
    TIMING_SWIFTSYNTAX_NS.fetch_add(t_ss.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let t_fb = Instant::now();
    let extractor = SwiftExtractor;
    let mut result = extractor.extract(source, file_path)?;
    enrich_fallback_result(source, file_path, &mut result)?;
    TIMING_TS_FALLBACK_NS.fetch_add(t_fb.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Ok(result)
}

fn restamp_indexstore_file_path(result: &mut ExtractionResult, file_path: &Path) {
    let file_path = file_path.to_path_buf();
    for node in &mut result.nodes {
        node.file = file_path.clone();
    }
    for edge in &mut result.edges {
        for provenance in &mut edge.provenance {
            provenance.file = file_path.clone();
        }
    }
}

fn enrich_fallback_result(
    source: &[u8],
    file_path: &Path,
    result: &mut ExtractionResult,
) -> anyhow::Result<()> {
    let has_swiftui = source_contains_swiftui_markers(source);
    let has_l10n = has_swiftui || source_contains_l10n_markers(source);
    let has_assets = source_contains_asset_markers(source);
    let needs_tree = has_swiftui || has_l10n || has_assets;

    if !needs_tree {
        return Ok(());
    }

    let t_parse = Instant::now();
    let tree = treesitter::parse_swift(source)?;
    TIMING_TS_PARSE_NS.fetch_add(t_parse.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let t_enrich = Instant::now();
    if has_swiftui {
        let _ = treesitter::enrich_swiftui_structure_with_tree(source, file_path, &tree, result);
    }
    if has_l10n {
        let _ =
            treesitter::enrich_localization_metadata_with_tree(source, file_path, &tree, result);
    }
    if has_assets {
        let _ = treesitter::enrich_asset_references_with_tree(source, file_path, &tree, result);
    }
    TIMING_TS_ENRICH_NS.fetch_add(t_enrich.elapsed().as_nanos() as u64, Ordering::Relaxed);

    Ok(())
}

#[doc(hidden)]
pub fn extract_swift_via_fallback_for_tests(
    source: &[u8],
    file_path: &Path,
) -> anyhow::Result<ExtractionResult> {
    let extractor = SwiftExtractor;
    let mut result = extractor.extract(source, file_path)?;
    enrich_fallback_result(source, file_path, &mut result)?;
    Ok(result)
}

#[cfg(test)]
mod plugin_tests {
    use super::{restamp_indexstore_file_path, stamp_swift_module};
    use grapha_core::ExtractionResult;
    use grapha_core::graph::{Edge, EdgeKind, EdgeProvenance, Node, NodeKind, Span, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn stamp_module_namespaces_package_manifest_ids() {
        let result = ExtractionResult {
            nodes: vec![Node {
                id: "s:4main7package18PackageDescription0C0Cvg".to_string(),
                kind: NodeKind::Function,
                name: "getter:package".to_string(),
                file: PathBuf::from("Package.swift"),
                span: Span {
                    start: [0, 0],
                    end: [0, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            }],
            edges: vec![Edge {
                source: "s:4main7package18PackageDescription0C0Cvg".to_string(),
                target: "external".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![EdgeProvenance {
                    file: PathBuf::from("Package.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [0, 0],
                    },
                    symbol_id: "s:4main7package18PackageDescription0C0Cvg".to_string(),
                }],
                repo: None,
            }],
            imports: vec![],
        };

        let stamped = stamp_swift_module(result, Some("Feature"));
        assert_eq!(
            stamped.nodes[0].id,
            "s:4main7package18PackageDescription0C0Cvg@@module:Feature"
        );
        assert_eq!(
            stamped.edges[0].source,
            "s:4main7package18PackageDescription0C0Cvg@@module:Feature"
        );
        assert_eq!(
            stamped.edges[0].provenance[0].symbol_id,
            "s:4main7package18PackageDescription0C0Cvg@@module:Feature"
        );
        assert_eq!(stamped.nodes[0].module.as_deref(), Some("Feature"));
    }

    #[test]
    fn restamps_indexstore_results_to_project_relative_file_path() {
        let mut result = ExtractionResult {
            nodes: vec![Node {
                id: "s:4Room0A4PageV".to_string(),
                kind: NodeKind::Struct,
                name: "RoomPage".to_string(),
                file: PathBuf::from("RoomPage.swift"),
                span: Span {
                    start: [20, 0],
                    end: [42, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: Some("Room".to_string()),
                snippet: None,
                repo: None,
            }],
            edges: vec![Edge {
                source: "s:4Room0A4PageV".to_string(),
                target: "s:4Room0A4PageV4bodyQrvp".to_string(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![EdgeProvenance {
                    file: PathBuf::from("RoomPage.swift"),
                    span: Span {
                        start: [20, 0],
                        end: [42, 0],
                    },
                    symbol_id: "s:4Room0A4PageV".to_string(),
                }],
                repo: None,
            }],
            imports: vec![],
        };

        let relative = std::path::Path::new("Modules/Room/Sources/Room/View/RoomPage.swift");
        restamp_indexstore_file_path(&mut result, relative);

        assert_eq!(result.nodes[0].file, relative);
        assert_eq!(result.edges[0].provenance[0].file, relative);
    }
}

#[cfg(test)]
mod discovery_cache_tests {
    use super::{
        index_store_path_in, init_index_store_with, prepare_project_with, project_cache_key,
        refresh_index_store_with,
    };
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::RwLock;

    #[test]
    fn normalizes_file_roots_to_parent_directory() {
        assert_eq!(
            project_cache_key(Path::new("/tmp/MyApp/Sources/File.swift")),
            std::path::PathBuf::from("/tmp/MyApp/Sources")
        );
    }

    #[test]
    fn keeps_directory_roots_stable() {
        assert_eq!(
            project_cache_key(Path::new("/tmp/MyApp")),
            std::path::PathBuf::from("/tmp/MyApp")
        );
    }

    #[test]
    fn caches_an_initial_miss() {
        let cache = RwLock::new(HashMap::new());
        let project = Path::new("/tmp/MyApp");
        let mut first_attempts = 0;
        let mut second_attempts = 0;

        assert_eq!(
            init_index_store_with(&cache, project, |_| {
                first_attempts += 1;
                None
            }),
            None
        );
        assert!(first_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project), None);

        assert_eq!(
            init_index_store_with(&cache, project, |_| {
                second_attempts += 1;
                Some(PathBuf::from("/tmp/DerivedData/Store"))
            }),
            None
        );
        assert_eq!(second_attempts, 0);
        assert_eq!(index_store_path_in(&cache, project), None);
    }

    #[test]
    fn explicit_refresh_allows_rediscovery_after_a_cached_miss() {
        let cache = RwLock::new(HashMap::new());
        let project = Path::new("/tmp/MyApp");
        let expected = PathBuf::from("/tmp/DerivedData/Store");
        let mut first_attempts = 0;
        let mut refresh_attempts = 0;

        assert_eq!(
            init_index_store_with(&cache, project, |_| {
                first_attempts += 1;
                None
            }),
            None
        );
        assert!(first_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project), None);

        assert_eq!(
            refresh_index_store_with(&cache, project, |_| {
                refresh_attempts += 1;
                Some(expected.clone())
            }),
            Some(expected.clone())
        );
        assert!(refresh_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project), Some(expected));
    }

    #[test]
    fn prepare_project_refreshes_a_cached_miss() {
        let cache = RwLock::new(HashMap::new());
        let project = Path::new("/tmp/MyApp");
        let expected = PathBuf::from("/tmp/DerivedData/Store");
        let mut initial_attempts = 0;
        let mut prepare_attempts = 0;

        assert_eq!(
            init_index_store_with(&cache, project, |_| {
                initial_attempts += 1;
                None
            }),
            None
        );
        assert!(initial_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project), None);

        prepare_project_with(&cache, project, |_| {
            prepare_attempts += 1;
            Some(expected.clone())
        });

        assert!(prepare_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project), Some(expected));
    }

    #[test]
    fn cached_miss_for_one_project_does_not_block_other_project_discovery() {
        let cache = RwLock::new(HashMap::new());
        let project_a = Path::new("/tmp/MyAppA");
        let project_b = Path::new("/tmp/MyAppB");
        let expected = PathBuf::from("/tmp/DerivedData/StoreB");
        let mut project_a_attempts = 0;
        let mut project_b_attempts = 0;

        assert_eq!(
            init_index_store_with(&cache, project_a, |_| {
                project_a_attempts += 1;
                None
            }),
            None
        );
        assert!(project_a_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project_a), None);

        assert_eq!(
            init_index_store_with(&cache, project_b, |_| {
                project_b_attempts += 1;
                Some(expected.clone())
            }),
            Some(expected.clone())
        );
        assert!(project_b_attempts > 0);
        assert_eq!(index_store_path_in(&cache, project_a), None);
        assert_eq!(index_store_path_in(&cache, project_b), Some(expected));
    }
}

#[cfg(test)]
mod index_store_toggle_tests {
    use super::extract_swift_with_init;
    use std::cell::Cell;
    use std::path::Path;

    #[test]
    fn disabled_index_store_skips_initializer() {
        let initializer_called = Cell::new(false);
        let source = br#"
        import SwiftUI

        struct ContentView: View {
            var body: some View {
                Text("Hello")
            }
        }
        "#;

        let result = extract_swift_with_init(
            source,
            Path::new("ContentView.swift"),
            None,
            None,
            false,
            |_| initializer_called.set(true),
        )
        .unwrap();

        assert!(!initializer_called.get());
        assert!(result.nodes.iter().any(|node| node.name == "ContentView"));
    }
}

#[cfg(test)]
mod marker_tests {
    use super::{
        extract_swift_via_fallback_for_tests, source_contains_asset_markers,
        source_contains_l10n_markers, source_contains_swiftui_markers,
    };
    use std::path::Path;

    #[test]
    fn swiftui_markers_ignore_plain_imports() {
        let source = br#"
        import SwiftUI

        struct Palette {
            let primary: Color
        }
        "#;

        assert!(!source_contains_swiftui_markers(source));
    }

    #[test]
    fn swiftui_markers_cover_view_signatures_and_builders() {
        let direct_view = br#"
        struct ContentView: View {
            var body: some View { Text("Hi") }
        }
        "#;
        assert!(source_contains_swiftui_markers(direct_view));

        let builder_helper = br#"
        @ViewBuilder
        func content() -> some View {
            Text("Hi")
        }
        "#;
        assert!(source_contains_swiftui_markers(builder_helper));

        let existential_view = br#"
        func erasedBody() -> any SwiftUI.View {
            fatalError()
        }
        "#;
        assert!(source_contains_swiftui_markers(existential_view));
    }

    #[test]
    fn localization_and_asset_markers_still_match_common_cases() {
        assert!(source_contains_l10n_markers(br#"Text("hello")"#));
        assert!(source_contains_l10n_markers(
            br#"Text(.accountForgetPassword)"#
        ));
        assert!(source_contains_l10n_markers(br#"Text(i18n: .commonShare)"#));
        assert!(source_contains_l10n_markers(
            br#"String(i18n: .commonShare)"#
        ));
        assert!(source_contains_l10n_markers(
            br#"NSLocalizedString("hello", comment: "")"#
        ));
        assert!(source_contains_asset_markers(br#"Image("logo")"#));
    }

    #[test]
    fn fallback_runs_l10n_enrichment_for_swiftui_custom_views_without_explicit_markers() {
        let source = br#"
        import SwiftUI

        struct TitleRow: View {
            let title: String

            var body: some View {
                Text(title)
            }
        }

        struct ContentView: View {
            let title: String

            var body: some View {
                TitleRow(title: title)
            }
        }
        "#;

        let result =
            extract_swift_via_fallback_for_tests(source, Path::new("ContentView.swift")).unwrap();

        assert!(
            result.nodes.iter().any(|node| {
                node.metadata
                    .get("l10n.ref_kind")
                    .is_some_and(|value| value == "possible_string")
            }),
            "SwiftUI files should still receive l10n enrichment even without explicit L10n markers"
        );
    }
}
