//! Stable, fixture-backed extraction-quality metrics for the generic
//! tree-sitter language set.
//!
//! The report deliberately measures raw per-file extraction results. That
//! isolates parser/walker quality from cross-file resolution. The same raw
//! results are also passed to merge's diagnostic API for the separately
//! reported unresolved-edge drop rate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use grapha_core::graph::{EdgeKind, NodeKind};
use grapha_core::{ExtractionResult, LanguageRegistry};
use serde::{Deserialize, Serialize};

const FIXTURE_LANGUAGES: &[&str] = &[
    "typescript",
    "tsx",
    "javascript",
    "python",
    "go",
    "java",
    "c",
    "cpp",
    "csharp",
    "php",
    "ruby",
    "kotlin",
    "dart",
    "pascal",
];

// Each entry names a declaration with a standard adjacent documentation form
// in the checked-in corpus. Node-kind quality is deliberately reported through
// the histogram rather than folded into documentation capture: a declaration
// can retain its doc comment even while its language-specific kind is still
// imperfect (for example, today's Go and Pascal fallbacks).
const DOCUMENTED_DECLARATIONS: &[(&str, &[&str])] = &[
    ("typescript", &["TypeScriptWorker"]),
    ("tsx", &["TsxWorker"]),
    ("javascript", &["JavaScriptWorker"]),
    ("python", &["PythonWorker"]),
    ("go", &["GoWorker"]),
    ("java", &["Main"]),
    ("c", &["CWorker"]),
    ("cpp", &["CppWorker"]),
    ("csharp", &["CSharpWorker"]),
    ("php", &["PhpWorker"]),
    ("ruby", &["RubyWorker"]),
    ("kotlin", &["KotlinWorker"]),
    ("dart", &["DartWorker"]),
    ("pascal", &["TWorker"]),
];

const BASELINE_DESCRIPTION: &str = "Generated from observed current known-good fixture outcomes; these values are a baseline, not aspirational P1 targets.";

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct QualityReport {
    schema_version: u8,
    baseline_description: String,
    languages: BTreeMap<String, LanguageMetrics>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct UnresolvedEdgeDropRate {
    input_edges: usize,
    dropped_unresolved_edges: usize,
    percent_of_input_edges: String,
    dropped_by_reason: BTreeMap<String, usize>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct LanguageMetrics {
    functions_with_outgoing_call_coverage: FunctionCallCoverage,
    documented_declaration_capture: DocumentationCapture,
    unresolved_edge_drop_rate: UnresolvedEdgeDropRate,
    node_kind_histogram: BTreeMap<String, usize>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct FunctionCallCoverage {
    functions_with_outgoing_calls: usize,
    functions_total: usize,
    percent: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct DocumentationCapture {
    documented_declarations_captured: usize,
    documented_declarations_total: usize,
    percent: String,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tree_sitter_quality")
}

fn registry() -> Result<LanguageRegistry> {
    let mut registry = LanguageRegistry::new();
    grapha_engine::rust_plugin::register_builtin(&mut registry)?;
    grapha_swift::register_builtin(&mut registry)?;
    grapha_engine::polyglot_plugin::register_builtin(&mut registry)?;
    Ok(registry)
}

fn fixture_language(root: &Path, file: &Path) -> Result<String> {
    let relative = file
        .strip_prefix(root)
        .with_context(|| format!("fixture file should be under {}", root.display()))?;
    let language = relative
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .with_context(|| {
            format!(
                "fixture file should have a language directory: {}",
                file.display()
            )
        })?;
    anyhow::ensure!(
        FIXTURE_LANGUAGES.contains(&language),
        "unexpected tree-sitter quality fixture language '{language}' in {}",
        file.display()
    );
    Ok(language.to_string())
}

fn collect_extractions(root: &Path) -> Result<BTreeMap<String, Vec<ExtractionResult>>> {
    for language in FIXTURE_LANGUAGES {
        anyhow::ensure!(
            root.join(language).is_dir(),
            "missing tree-sitter quality fixture directory: {language}"
        );
    }

    let registry = registry()?;
    let mut project = grapha_core::project_context(root);
    // The corpus measures the deterministic tree-sitter paths, not any local
    // Xcode index store that happens to exist on the test machine.
    project.index_store_enabled = false;
    grapha_core::prepare_plugins(&registry, &project)?;

    let extracted = (|| -> Result<BTreeMap<String, Vec<ExtractionResult>>> {
        let modules = grapha_core::discover_modules(&registry, &project)?;
        let mut files = grapha_core::pipeline::discover_files(root, &registry)?;
        files.sort();

        let mut by_language = FIXTURE_LANGUAGES
            .iter()
            .map(|language| ((*language).to_string(), Vec::new()))
            .collect::<BTreeMap<_, _>>();

        for file in files {
            let language = fixture_language(root, &file)?;
            let source = fs::read(&file)
                .with_context(|| format!("failed to read fixture source {}", file.display()))?;
            let context = grapha_core::file_context(&project, &modules, &file);
            let result = grapha_core::extract_with_registry(&registry, &source, &context)
                .with_context(|| format!("failed to extract fixture source {}", file.display()))?;
            by_language
                .get_mut(&language)
                .expect("fixture language was initialized")
                .push(result);
        }

        for (language, results) in &by_language {
            anyhow::ensure!(
                !results.is_empty(),
                "fixture language '{language}' did not yield a source file"
            );
        }
        Ok(by_language)
    })();

    let finished = grapha_core::finish_plugins(&registry, &project);
    let extractions = extracted?;
    finished?;
    Ok(extractions)
}

fn documented_declarations(language: &str) -> &[&'static str] {
    DOCUMENTED_DECLARATIONS
        .iter()
        .find_map(|(fixture_language, declarations)| {
            (*fixture_language == language).then_some(*declarations)
        })
        .unwrap_or_else(|| panic!("missing documented-declaration contract for {language}"))
}

fn percent(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        return "0.00%".to_string();
    }

    // Integer basis-points calculation keeps the serialized baseline stable
    // while still reporting a conventional percentage with two decimals.
    let basis_points = (numerator * 10_000 + denominator / 2) / denominator;
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

fn node_kind_name(kind: NodeKind) -> String {
    serde_json::to_value(kind)
        .expect("NodeKind should serialize")
        .as_str()
        .expect("NodeKind should serialize as a string")
        .to_string()
}

fn language_metrics(language: &str, results: &[ExtractionResult]) -> LanguageMetrics {
    let function_ids = results
        .iter()
        .flat_map(|result| result.nodes.iter())
        .filter(|node| node.kind == NodeKind::Function)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let outgoing_call_sources = results
        .iter()
        .flat_map(|result| result.edges.iter())
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .map(|edge| edge.source.clone())
        .collect::<BTreeSet<_>>();
    let functions_with_outgoing_calls = function_ids.intersection(&outgoing_call_sources).count();

    let documented = documented_declarations(language);
    let mut documented_names = BTreeSet::new();
    for declaration in documented {
        assert!(
            documented_names.insert(*declaration),
            "duplicate documented-declaration contract for {language}: {declaration}"
        );
    }
    let documented_declarations_captured = documented_names
        .iter()
        .filter(|expected_name| {
            results
                .iter()
                .flat_map(|result| result.nodes.iter())
                .any(|node| {
                    node.name == **expected_name
                        && node
                            .doc_comment
                            .as_deref()
                            .is_some_and(|comment| !comment.trim().is_empty())
                })
        })
        .count();

    // Every language directory is a self-contained fixture project, including
    // any sibling imports. Merging one directory at a time therefore keeps the
    // diagnostics attributable to that language without cross-language noise.
    let merge_stats = grapha_core::merge::merge_with_report(results.to_vec()).stats;
    let dropped_by_reason = merge_stats
        .dropped_unresolved_edge_count_by_reason
        .into_iter()
        .map(|(reason, count)| (unresolved_drop_reason_name(reason).to_string(), count))
        .collect();

    let mut node_kind_histogram = BTreeMap::new();
    for node in results.iter().flat_map(|result| result.nodes.iter()) {
        *node_kind_histogram
            .entry(node_kind_name(node.kind))
            .or_insert(0) += 1;
    }

    LanguageMetrics {
        functions_with_outgoing_call_coverage: FunctionCallCoverage {
            functions_with_outgoing_calls,
            functions_total: function_ids.len(),
            percent: percent(functions_with_outgoing_calls, function_ids.len()),
        },
        documented_declaration_capture: DocumentationCapture {
            documented_declarations_captured,
            documented_declarations_total: documented_names.len(),
            percent: percent(documented_declarations_captured, documented_names.len()),
        },
        unresolved_edge_drop_rate: UnresolvedEdgeDropRate {
            input_edges: merge_stats.input_edge_count,
            dropped_unresolved_edges: merge_stats.dropped_unresolved_edge_count,
            percent_of_input_edges: percent(
                merge_stats.dropped_unresolved_edge_count,
                merge_stats.input_edge_count,
            ),
            dropped_by_reason,
        },
        node_kind_histogram,
    }
}

fn unresolved_drop_reason_name(
    reason: grapha_core::merge::UnresolvedEdgeDropReason,
) -> &'static str {
    match reason {
        grapha_core::merge::UnresolvedEdgeDropReason::NoCandidate => "no_candidate",
        grapha_core::merge::UnresolvedEdgeDropReason::AmbiguousMoreThanThreeFiles => {
            "ambiguous_more_than_three_files"
        }
    }
}

fn current_report() -> Result<QualityReport> {
    let extractions = collect_extractions(&fixture_root())?;
    let languages = extractions
        .iter()
        .map(|(language, results)| (language.clone(), language_metrics(language, results)))
        .collect();

    Ok(QualityReport {
        schema_version: 1,
        baseline_description: BASELINE_DESCRIPTION.to_string(),
        languages,
    })
}

#[test]
fn tree_sitter_quality_metrics_match_checked_in_baseline() -> Result<()> {
    let actual = current_report()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&actual).expect("quality report should serialize")
    );

    let expected: QualityReport =
        serde_json::from_str(include_str!("fixtures/tree_sitter_quality/baselines.json"))
            .context("tree-sitter quality baseline should be valid JSON")?;
    assert_eq!(
        actual, expected,
        "tree-sitter fixture metrics changed; inspect the printed observed report and intentionally update baselines.json for a verified behavior change"
    );
    Ok(())
}
