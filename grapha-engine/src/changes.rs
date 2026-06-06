use std::collections::{HashMap, HashSet};
use std::path::Path;

use git2::{DiffOptions, Repository};
use serde::Serialize;

use crate::query::impact;
use grapha_core::graph::Graph;

/// Cap for the `affected_symbols` vector returned by
/// [`detect_changes_with_options`]. Each contained `ImpactResult` is also
/// produced with the same per-bucket cap so AI-agent callers can keep total
/// output bounded across nested vectors.
///
/// The default leaves results unbounded (matching prior behavior); the CLI
/// overrides this with the user-supplied `--limit`.
#[derive(Debug, Clone)]
pub struct ChangeQueryOptions {
    pub limit: usize,
}

impl Default for ChangeQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

#[derive(Debug, Serialize)]
pub struct ChangeReport {
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<ChangedSymbol>,
    pub affected_symbols: Vec<impact::ImpactResult>,
    pub total_affected_symbols: usize,
    pub risk_summary: RiskSummary,
}

#[derive(Debug, Serialize)]
pub struct ChangedSymbol {
    pub id: String,
    pub name: String,
    pub file: String,
}

/// Headline impact counts derived from the FULL changed-symbol set, BEFORE
/// `--limit` truncates [`ChangeReport::affected_symbols`].
///
/// `directly_affected` and `transitively_affected` are summed across the
/// pre-truncation impact totals (`total_depth_1` / `total_affected`) so the
/// risk level reflects the actual blast radius even when the visible
/// `affected_symbols` vector has been capped for token-budget reasons. As a
/// result, these counts may exceed
/// `affected_symbols.iter().map(|s| s.depth_1.len()).sum()` when truncation is
/// in effect.
#[derive(Debug, Serialize)]
pub struct RiskSummary {
    /// Number of changed symbols that anchored the impact analysis.
    pub changed_count: usize,
    /// Sum of `total_depth_1` across all changed symbols. Pre-`--limit`.
    pub directly_affected: usize,
    /// Sum of `total_affected` across all changed symbols. Pre-`--limit`.
    pub transitively_affected: usize,
    /// `"low"`, `"medium"`, or `"high"` derived from `transitively_affected`.
    pub risk_level: String,
}

pub fn detect_changes_with_options(
    repo_path: &Path,
    graph: &Graph,
    scope: &str,
    options: &ChangeQueryOptions,
) -> anyhow::Result<ChangeReport> {
    let repo = Repository::discover(repo_path)?;

    let changed_hunks = match scope {
        "unstaged" => diff_unstaged(&repo)?,
        "staged" => diff_staged(&repo)?,
        "all" => {
            let mut hunks = diff_unstaged(&repo)?;
            hunks.extend(diff_staged(&repo)?);
            hunks
        }
        base_ref => diff_against_ref(&repo, base_ref)?,
    };

    let changed_files: Vec<String> = changed_hunks
        .iter()
        .map(|h| h.file.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let changed_symbols = collect_changed_symbols(&changed_hunks, graph);

    let impact_options = impact::ImpactQueryOptions {
        limit: options.limit,
    };
    let mut affected_symbols = Vec::new();
    for sym in &changed_symbols {
        let impact_result = impact::query_impact_with_options(graph, &sym.id, 3, &impact_options)
            .map_err(|error| {
            anyhow::anyhow!(
                "failed to resolve changed symbol {} during impact analysis: {error}",
                sym.id
            )
        })?;
        affected_symbols.push(impact_result);
    }

    // Compute risk-summary totals against the pre-truncation impact totals so the
    // headline counts don't drop when `--limit` is small.
    let directly_affected: usize = affected_symbols.iter().map(|r| r.total_depth_1).sum();
    let transitively_affected: usize = affected_symbols.iter().map(|r| r.total_affected).sum();

    let risk_level = if transitively_affected > 20 {
        "high"
    } else if transitively_affected > 5 {
        "medium"
    } else {
        "low"
    }
    .to_string();

    let changed_count = changed_symbols.len();

    let total_affected_symbols =
        crate::query::truncate_with_total(&mut affected_symbols, options.limit);

    Ok(ChangeReport {
        changed_files,
        changed_symbols,
        affected_symbols,
        total_affected_symbols,
        risk_summary: RiskSummary {
            changed_count,
            directly_affected,
            transitively_affected,
            risk_level,
        },
    })
}

struct Hunk {
    file: String,
    start_line: usize,
    end_line: usize,
}

fn collect_changed_symbols(changed_hunks: &[Hunk], graph: &Graph) -> Vec<ChangedSymbol> {
    let node_index: HashMap<&str, &grapha_core::graph::Node> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut changed_symbols = Vec::new();
    let mut seen_ids = HashSet::new();

    for hunk in changed_hunks {
        for node in &graph.nodes {
            let node_file = node.file.to_string_lossy();
            if node_file.as_ref() == hunk.file
                && ranges_overlap(
                    hunk.start_line,
                    hunk.end_line,
                    node.span.start[0],
                    node.span.end[0],
                )
                && seen_ids.insert(node.id.clone())
            {
                changed_symbols.push(ChangedSymbol {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    file: hunk.file.clone(),
                });
            }
        }

        for edge in &graph.edges {
            if edge.provenance.iter().any(|provenance| {
                provenance.file.to_string_lossy().as_ref() == hunk.file
                    && ranges_overlap(
                        hunk.start_line,
                        hunk.end_line,
                        provenance.span.start[0],
                        provenance.span.end[0],
                    )
            }) && let Some(source_node) = node_index.get(edge.source.as_str())
                && seen_ids.insert(source_node.id.clone())
            {
                changed_symbols.push(ChangedSymbol {
                    id: source_node.id.clone(),
                    name: source_node.name.clone(),
                    file: hunk.file.clone(),
                });
            }
        }
    }

    changed_symbols
}

fn ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start <= b_end && b_start <= a_end
}

fn diff_unstaged(repo: &Repository) -> anyhow::Result<Vec<Hunk>> {
    let mut opts = DiffOptions::new();
    let diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;
    extract_hunks(&diff)
}

fn diff_staged(repo: &Repository) -> anyhow::Result<Vec<Hunk>> {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = DiffOptions::new();
    let diff = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?;
    extract_hunks(&diff)
}

fn diff_against_ref(repo: &Repository, refspec: &str) -> anyhow::Result<Vec<Hunk>> {
    let obj = repo.revparse_single(refspec)?;
    let tree = obj.peel_to_tree()?;
    let mut opts = DiffOptions::new();
    let diff = repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts))?;
    extract_hunks(&diff)
}

fn extract_hunks(diff: &git2::Diff) -> anyhow::Result<Vec<Hunk>> {
    let mut hunks = Vec::new();

    diff.foreach(
        &mut |_delta, _progress| true,
        None,
        Some(&mut |delta, hunk| {
            if let Some(path) = delta.new_file().path().and_then(|p| p.to_str()) {
                hunks.push(Hunk {
                    file: path.to_string(),
                    start_line: hunk.new_start() as usize,
                    end_line: (hunk.new_start() + hunk.new_lines()) as usize,
                });
            }
            true
        }),
        None,
    )?;

    Ok(hunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{
        Edge, EdgeKind, EdgeProvenance, Graph, Node, NodeKind, Span, Visibility,
    };
    use std::path::PathBuf;

    #[test]
    fn ranges_overlap_works() {
        assert!(ranges_overlap(0, 10, 5, 15));
        assert!(ranges_overlap(5, 15, 0, 10));
        assert!(!ranges_overlap(0, 5, 10, 15));
        assert!(ranges_overlap(0, 10, 10, 20));
    }

    #[test]
    fn collect_changed_symbols_matches_edge_provenance() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![Node {
                id: "src/lib.rs::handler".to_string(),
                kind: NodeKind::Function,
                name: "handler".to_string(),
                file: PathBuf::from("src/lib.rs"),
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
            }],
            edges: vec![Edge {
                source: "src/lib.rs::handler".to_string(),
                target: "src/lib.rs::db_call".to_string(),
                kind: EdgeKind::Calls,
                confidence: 0.9,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![EdgeProvenance {
                    file: PathBuf::from("src/lib.rs"),
                    span: Span {
                        start: [8, 4],
                        end: [8, 20],
                    },
                    symbol_id: "src/lib.rs::handler".to_string(),
                }],
                repo: None,
            }],
        };

        let changed_symbols = collect_changed_symbols(
            &[Hunk {
                file: "src/lib.rs".to_string(),
                start_line: 8,
                end_line: 8,
            }],
            &graph,
        );

        assert_eq!(changed_symbols.len(), 1);
        assert_eq!(changed_symbols[0].id, "src/lib.rs::handler");
    }

    #[test]
    fn changes_truncates_affected_and_propagates_limit_to_impact() {
        use git2::{IndexAddOption, Signature};
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init repo");

        // Initial commit of empty a.rs / b.rs / c.rs so diff_index_to_workdir
        // has a baseline to diff against. The unstaged workdir then contains
        // the actual content the test expects.
        let signature = Signature::now("Test", "test@example.com").expect("create signature");
        for name in ["a.rs", "b.rs", "c.rs"] {
            fs::write(dir.path().join(name), "").expect("write empty baseline");
        }
        let mut index = repo.index().expect("repo index");
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .expect("stage baseline");
        index.write().expect("write index");
        let tree_oid = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_oid).expect("find tree");
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .expect("commit");

        // Now write three single-line files; the unstaged diff will show one
        // hunk per file covering line 1.
        for name in ["a.rs", "b.rs", "c.rs"] {
            fs::write(dir.path().join(name), "fn changed() {}\n").expect("write changed file");
        }

        let mk_node = |id: &str, file: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 0],
                end: [1, 16],
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
        let calls = |source: &str, target: &str| Edge {
            source: source.into(),
            target: target.into(),
            kind: EdgeKind::Calls,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };

        // Three changed symbols (one per file). The first changed symbol
        // ("a_changed") has three direct callers so we can detect that the
        // per-bucket cap was forwarded into query_impact_with_options.
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk_node("a_changed", "a.rs"),
                mk_node("b_changed", "b.rs"),
                mk_node("c_changed", "c.rs"),
                mk_node("caller_1", "callers.rs"),
                mk_node("caller_2", "callers.rs"),
                mk_node("caller_3", "callers.rs"),
            ],
            edges: vec![
                calls("caller_1", "a_changed"),
                calls("caller_2", "a_changed"),
                calls("caller_3", "a_changed"),
            ],
        };

        let unbounded = detect_changes_with_options(
            dir.path(),
            &graph,
            "unstaged",
            &ChangeQueryOptions::default(),
        )
        .expect("detect_changes unbounded");
        assert_eq!(unbounded.changed_symbols.len(), 3);
        assert_eq!(unbounded.affected_symbols.len(), 3);
        assert_eq!(unbounded.total_affected_symbols, 3);
        // For "a_changed", depth_1 should contain all three callers.
        let unbounded_a = unbounded
            .affected_symbols
            .iter()
            .find(|r| r.source == "a_changed")
            .expect("unbounded result for a_changed");
        assert_eq!(unbounded_a.depth_1.len(), 3);
        assert_eq!(unbounded_a.total_depth_1, 3);

        let truncated = detect_changes_with_options(
            dir.path(),
            &graph,
            "unstaged",
            &ChangeQueryOptions { limit: 1 },
        )
        .expect("detect_changes truncated");
        assert_eq!(
            truncated.affected_symbols.len(),
            1,
            "affected_symbols should be truncated to limit"
        );
        assert_eq!(truncated.total_affected_symbols, 3);
        // The single retained ImpactResult must itself have been computed with
        // the same per-bucket cap forwarded into ImpactQueryOptions.
        let surviving = &truncated.affected_symbols[0];
        assert!(
            surviving.depth_1.len() <= 1,
            "nested ImpactResult should respect the same per-bucket cap, got {}",
            surviving.depth_1.len()
        );
        // If the surviving entry is the high-fan-out one, the pre-truncation
        // total should still report 3 even though the visible vec is capped.
        if surviving.source == "a_changed" {
            assert_eq!(surviving.total_depth_1, 3);
        }
    }

    #[test]
    fn risk_summary_counts_full_changeset_not_truncated_view() {
        // Contract: `risk_summary.directly_affected` is the sum of
        // `total_depth_1` across the FULL pre-truncation changeset. When
        // `--limit` shrinks `affected_symbols`, this headline count must NOT
        // drop to match the post-truncation view.
        use git2::{IndexAddOption, Signature};
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init repo");

        let signature = Signature::now("Test", "test@example.com").expect("create signature");
        for name in ["a.rs", "b.rs", "c.rs"] {
            fs::write(dir.path().join(name), "").expect("write empty baseline");
        }
        let mut index = repo.index().expect("repo index");
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .expect("stage baseline");
        index.write().expect("write index");
        let tree_oid = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_oid).expect("find tree");
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .expect("commit");

        for name in ["a.rs", "b.rs", "c.rs"] {
            fs::write(dir.path().join(name), "fn changed() {}\n").expect("write changed file");
        }

        let mk_node = |id: &str, file: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 0],
                end: [1, 16],
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
        let calls = |source: &str, target: &str| Edge {
            source: source.into(),
            target: target.into(),
            kind: EdgeKind::Calls,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };

        // 3 changed symbols, each with 2 direct callers => directly_affected=6
        // and transitively_affected=6 in the full pre-truncation changeset.
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk_node("a_changed", "a.rs"),
                mk_node("b_changed", "b.rs"),
                mk_node("c_changed", "c.rs"),
                mk_node("caller_a1", "callers.rs"),
                mk_node("caller_a2", "callers.rs"),
                mk_node("caller_b1", "callers.rs"),
                mk_node("caller_b2", "callers.rs"),
                mk_node("caller_c1", "callers.rs"),
                mk_node("caller_c2", "callers.rs"),
            ],
            edges: vec![
                calls("caller_a1", "a_changed"),
                calls("caller_a2", "a_changed"),
                calls("caller_b1", "b_changed"),
                calls("caller_b2", "b_changed"),
                calls("caller_c1", "c_changed"),
                calls("caller_c2", "c_changed"),
            ],
        };

        // Unbounded: headline count should equal sum-of-depth_1 over visible
        // affected_symbols. Establishes the baseline.
        let unbounded = detect_changes_with_options(
            dir.path(),
            &graph,
            "unstaged",
            &ChangeQueryOptions::default(),
        )
        .expect("detect_changes unbounded");
        let unbounded_visible_direct: usize = unbounded
            .affected_symbols
            .iter()
            .map(|s| s.depth_1.len())
            .sum();
        assert_eq!(unbounded.risk_summary.directly_affected, 6);
        assert_eq!(unbounded.risk_summary.transitively_affected, 6);
        assert_eq!(unbounded.risk_summary.changed_count, 3);
        assert_eq!(
            unbounded.risk_summary.directly_affected, unbounded_visible_direct,
            "unbounded headline must equal the visible sum when nothing is truncated"
        );

        // Truncated: only 1 affected_symbol survives, so the visible
        // sum-of-depth_1 is at most 1. The headline must still report 6.
        let truncated = detect_changes_with_options(
            dir.path(),
            &graph,
            "unstaged",
            &ChangeQueryOptions { limit: 1 },
        )
        .expect("detect_changes truncated");
        assert_eq!(truncated.affected_symbols.len(), 1);
        assert_eq!(truncated.total_affected_symbols, 3);

        let truncated_visible_direct: usize = truncated
            .affected_symbols
            .iter()
            .map(|s| s.depth_1.len())
            .sum();
        assert!(
            truncated_visible_direct <= 1,
            "post-truncation visible sum must be at most limit=1, got {truncated_visible_direct}"
        );
        // Invariant: the headline reflects the full pre-truncation changeset.
        assert_eq!(
            truncated.risk_summary.directly_affected, 6,
            "directly_affected must be the full pre-truncation count"
        );
        assert_eq!(
            truncated.risk_summary.transitively_affected, 6,
            "transitively_affected must be the full pre-truncation count"
        );
        assert_eq!(
            truncated.risk_summary.changed_count, 3,
            "changed_count reflects all changed symbols, not the truncated view"
        );
        // Headline strictly exceeds the visible sum when truncation drops rows.
        assert!(
            truncated.risk_summary.directly_affected > truncated_visible_direct,
            "headline directly_affected ({}) must exceed truncated visible sum ({}) under --limit",
            truncated.risk_summary.directly_affected,
            truncated_visible_direct,
        );
    }
}
