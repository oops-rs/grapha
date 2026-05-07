use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::classify::{CompositeClassifier, classify_extraction_result, classify_graph};
use crate::discover;
use crate::extract::ExtractionResult;
use crate::graph::Graph;
use crate::merge;
use crate::module::ModuleMap;
use crate::normalize::normalize_graph;
use crate::plugin::{FileContext, GraphPass, LanguageRegistry, ProjectContext};
use crate::semantic::SemanticDocument;

pub fn project_context(path: &Path) -> ProjectContext {
    ProjectContext::new(path)
}

pub fn discover_files(path: &Path, registry: &LanguageRegistry) -> anyhow::Result<Vec<PathBuf>> {
    discover::discover_files(path, &registry.supported_extensions())
}

pub fn relative_path_for_input(input_path: &Path, file: &Path) -> PathBuf {
    if input_path.is_dir() {
        if let (Ok(root), Ok(file)) = (
            std::fs::canonicalize(input_path),
            std::fs::canonicalize(file),
        ) && let Ok(relative) = file.strip_prefix(&root)
        {
            return relative.to_path_buf();
        }

        file.strip_prefix(input_path).unwrap_or(file).to_path_buf()
    } else {
        file.file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| file.to_path_buf())
    }
}

pub fn prepare_plugins(
    registry: &LanguageRegistry,
    context: &ProjectContext,
) -> anyhow::Result<()> {
    registry.prepare_plugins(context)
}

pub fn finish_plugins(registry: &LanguageRegistry, context: &ProjectContext) -> anyhow::Result<()> {
    registry.finish_plugins(context)
}

pub fn discover_modules(
    registry: &LanguageRegistry,
    context: &ProjectContext,
) -> anyhow::Result<ModuleMap> {
    let mut modules = ModuleMap::new();
    for plugin in registry.plugins() {
        modules.merge(
            plugin
                .discover_modules(context)
                .with_context(|| format!("failed to discover modules for '{}'", plugin.id()))?,
        );
    }
    Ok(modules.with_fallback(&context.project_root))
}

pub fn file_context(context: &ProjectContext, modules: &ModuleMap, file: &Path) -> FileContext {
    let relative_path = relative_path_for_input(&context.input_path, file);
    let absolute_path =
        std::fs::canonicalize(file).unwrap_or_else(|_| context.project_root.join(&relative_path));
    let module_name = modules.module_for_file(&absolute_path).or_else(|| {
        relative_path
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .map(|segment| segment.to_string())
    });

    FileContext {
        input_path: context.input_path.clone(),
        project_root: context.project_root.clone(),
        relative_path,
        absolute_path,
        module_name,
        index_store_enabled: context.index_store_enabled,
    }
}

pub fn extract_with_registry(
    registry: &LanguageRegistry,
    source: &[u8],
    context: &FileContext,
) -> anyhow::Result<ExtractionResult> {
    Ok(lower_semantics(extract_semantics_with_registry(
        registry, source, context,
    )?))
}

pub fn extract_semantics_with_registry(
    registry: &LanguageRegistry,
    source: &[u8],
    context: &FileContext,
) -> anyhow::Result<SemanticDocument> {
    let plugin = registry.plugin_for_path(&context.relative_path)?;
    let document = plugin.extract_semantics(source, context)?;
    Ok(plugin.stamp_semantic_module(document, context.module_name.as_deref()))
}

pub fn stamp_module(result: ExtractionResult, module_name: Option<&str>) -> ExtractionResult {
    let Some(module_name) = module_name else {
        return result;
    };

    let nodes = result
        .nodes
        .into_iter()
        .map(|mut node| {
            node.module = Some(module_name.to_string());
            node
        })
        .collect();

    ExtractionResult {
        nodes,
        edges: result.edges,
        imports: result.imports,
    }
}

pub fn stamp_semantic_module(
    document: SemanticDocument,
    module_name: Option<&str>,
) -> SemanticDocument {
    document.stamp_module(module_name)
}

pub fn lower_semantics(document: SemanticDocument) -> ExtractionResult {
    document.into_extraction_result()
}

pub fn build_graph(
    results: Vec<ExtractionResult>,
    classifier: &CompositeClassifier,
    graph_passes: &[Box<dyn GraphPass>],
) -> Graph {
    let preclassified_results = results
        .into_iter()
        .map(|result| classify_extraction_result(result, classifier))
        .collect();
    let mut graph = classify_graph(&merge::merge(preclassified_results), classifier);
    for pass in graph_passes {
        graph = pass.apply(graph);
    }
    normalize_graph(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Classification, Classifier};
    use crate::extract::ExtractionResult;
    use crate::graph::{
        Edge, EdgeKind, FlowDirection, Graph, Node, NodeKind, NodeRole, Span, TerminalKind,
        Visibility,
    };
    use crate::plugin::{GraphPass, LanguagePlugin};
    use std::collections::HashMap;

    struct TestPlugin;

    impl LanguagePlugin for TestPlugin {
        fn id(&self) -> &'static str {
            "test"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["rs"]
        }

        fn extract(
            &self,
            _source: &[u8],
            context: &FileContext,
        ) -> anyhow::Result<ExtractionResult> {
            let mut result = ExtractionResult::new();
            result.nodes.push(Node {
                id: context.relative_path.to_string_lossy().to_string(),
                kind: NodeKind::Function,
                name: "main".to_string(),
                file: context.relative_path.clone(),
                span: Span {
                    start: [0, 0],
                    end: [1, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            });
            Ok(result)
        }
    }

    struct NetworkClassifier;

    impl Classifier for NetworkClassifier {
        fn classify(
            &self,
            _call_target: &str,
            _context: &crate::classify::ClassifyContext,
        ) -> Option<Classification> {
            Some(Classification {
                terminal_kind: TerminalKind::Network,
                direction: FlowDirection::Read,
                operation: "HTTP".to_string(),
            })
        }
    }

    struct EntryPass;

    impl GraphPass for EntryPass {
        fn apply(&self, mut graph: Graph) -> Graph {
            if let Some(node) = graph.nodes.first_mut() {
                node.role = Some(NodeRole::EntryPoint);
            }
            graph
        }
    }

    #[test]
    fn extract_with_registry_stamps_module() {
        let mut registry = LanguageRegistry::new();
        registry.register(TestPlugin).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file = src_dir.join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let project = ProjectContext {
            input_path: dir.path().to_path_buf(),
            project_root: dir.path().to_path_buf(),
            index_store_enabled: true,
        };
        let mut modules = ModuleMap::new();
        modules.modules.insert("core".to_string(), vec![src_dir]);
        modules = modules.with_fallback(dir.path());
        let file_context = file_context(&project, &modules, &file);

        let result = extract_with_registry(&registry, b"fn main() {}", &file_context).unwrap();
        assert_eq!(result.nodes[0].module.as_deref(), Some("core"));
    }

    #[test]
    fn relative_path_for_input_uses_project_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file = src_dir.join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        assert_eq!(
            relative_path_for_input(dir.path(), &file),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn build_graph_runs_classifier_then_graph_pass() {
        let node = Node {
            id: "src::main".to_string(),
            kind: NodeKind::Function,
            name: "main".to_string(),
            file: PathBuf::from("main.rs"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };
        let result = ExtractionResult {
            nodes: vec![node],
            edges: vec![Edge {
                source: "src::main".to_string(),
                target: "reqwest::get".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: Vec::new(),
        };
        let classifier = CompositeClassifier::new(vec![Box::new(NetworkClassifier)]);
        let graph = build_graph(vec![result], &classifier, &[Box::new(EntryPass)]);

        assert_eq!(graph.edges[0].direction, Some(FlowDirection::Read));
        assert_eq!(graph.nodes[0].role, Some(NodeRole::EntryPoint));
    }
}
