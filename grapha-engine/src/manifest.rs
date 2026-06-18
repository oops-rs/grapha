//! Portable code-index artifact manifest (ADR-0027, Slice 3a, producer side).
//!
//! On `grapha index` we additively emit a `manifest.json` next to the `.grapha`
//! store. It generalizes the [`crate::remote`] `ProjectRevisionMetadata`
//! precedent (producer / `schema_version` / `source_revision`) into the
//! two-axis capability manifest a downstream consumer (nous) later ingests.
//!
//! This module is **producer-only and additive**: it never changes how the
//! store is opened or queried, and writing the manifest is best-effort relative
//! to the existing index pipeline. Signing (`manifest.sig`) and any consumer
//! verification are deferred to the Slice 3 spec — we emit the unsigned
//! manifest plus self-computed digests only.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use grapha_core::graph::Graph;

/// The producer name stamped into every grapha-emitted manifest.
pub const PRODUCER: &str = "grapha";

/// Manifest schema version. Manifest fields are additive-only across versions
/// (matching the `remote.rs` `bundle_schema_version` discipline); a consumer
/// allowlists known versions and rejects unknown ones.
pub const MANIFEST_SCHEMA_VERSION: &str = "1";

/// The filename written next to the `.grapha` store.
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// How a source path is expressed in the artifact. The portable artifact
/// always uses `relative` so it carries no absolute host paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePathMode {
    Relative,
    Absolute,
}

/// First capability axis: how good the operation is for this artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Full,
    Partial,
    Unsupported,
}

/// Second capability axis: where the answer is served from. This is a *where*,
/// not a *quality* — the two axes are orthogonal and never collapsed into a
/// single flat enum (ADR-0027 Decision 5 rejects `full|partial|unsupported|source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServedBy {
    Graph,
    Source,
}

/// The two-axis capability for a single op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub availability: Availability,
    pub served_by: ServedBy,
}

impl Capability {
    const fn graph(availability: Availability) -> Self {
        Self {
            availability,
            served_by: ServedBy::Graph,
        }
    }

    const fn source(availability: Availability) -> Self {
        Self {
            availability,
            served_by: ServedBy::Source,
        }
    }
}

/// The fixed op vocabulary. `served_by` is intrinsic to the op:
/// `read_file`/`grep` are served from source, everything else from the graph
/// cache, and `codebases` is always available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    SearchSymbols,
    Context,
    Usages,
    Impact,
    Trace,
    Concept,
    Dependents,
    ReadFile,
    Grep,
    Codebases,
}

impl Op {
    pub const ALL: [Op; 10] = [
        Op::SearchSymbols,
        Op::Context,
        Op::Usages,
        Op::Impact,
        Op::Trace,
        Op::Concept,
        Op::Dependents,
        Op::ReadFile,
        Op::Grep,
        Op::Codebases,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Op::SearchSymbols => "search_symbols",
            Op::Context => "context",
            Op::Usages => "usages",
            Op::Impact => "impact",
            Op::Trace => "trace",
            Op::Concept => "concept",
            Op::Dependents => "dependents",
            Op::ReadFile => "read_file",
            Op::Grep => "grep",
            Op::Codebases => "codebases",
        }
    }
}

/// The per-language quality tier grapha provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageTier {
    /// Deep, language-aware extraction (Rust via grapha-rust, Swift via
    /// libIndexStore + SwiftSyntax): full graph ops.
    Deep,
    /// Tree-sitter-only structural extraction: symbol/context/usages are
    /// partial, and the deeper semantic graph ops (concept/impact/trace) are
    /// partial or unsupported.
    TreeSitter,
}

/// Map a grapha language id (the `LanguagePlugin::id` / tree-sitter config id)
/// to its capability tier. `rust` and `swift` are deep; the polyglot
/// tree-sitter languages are best-effort.
pub fn language_tier(language: &str) -> LanguageTier {
    match language {
        "rust" | "swift" => LanguageTier::Deep,
        _ => LanguageTier::TreeSitter,
    }
}

/// The two-axis capability matrix for a single language. This is the
/// source-of-truth grapha publishes and nous consumes (ADR-0027 Decision 7).
///
/// `read_file`/`grep` are always `full`/`source` (they read the materialized
/// source tree, not the graph) and `codebases` is always `full`/`graph`.
pub fn capabilities_for_language(language: &str) -> BTreeMap<String, Capability> {
    let tier = language_tier(language);
    let graph_op = match tier {
        LanguageTier::Deep => Availability::Full,
        LanguageTier::TreeSitter => Availability::Partial,
    };
    // Deeper semantic ops degrade further on tree-sitter-only languages.
    let deep_semantic = match tier {
        LanguageTier::Deep => Availability::Full,
        LanguageTier::TreeSitter => Availability::Unsupported,
    };

    let mut map = BTreeMap::new();
    for op in Op::ALL {
        let capability = match op {
            Op::SearchSymbols | Op::Context | Op::Usages | Op::Dependents => {
                Capability::graph(graph_op)
            }
            Op::Concept | Op::Impact | Op::Trace => Capability::graph(deep_semantic),
            Op::ReadFile | Op::Grep => Capability::source(Availability::Full),
            Op::Codebases => Capability::graph(Availability::Full),
        };
        map.insert(op.as_str().to_string(), capability);
    }
    map
}

/// Merge per-language capability matrices into a single artifact-level matrix.
/// For each op the best (highest) availability across the artifact's languages
/// wins, so a Rust+TypeScript artifact reports `impact` as `full` because at
/// least one language supports it deeply.
pub fn merge_capabilities(languages: &[String]) -> BTreeMap<String, Capability> {
    let mut merged: BTreeMap<String, Capability> = BTreeMap::new();
    for language in languages {
        for (op, capability) in capabilities_for_language(language) {
            merged
                .entry(op)
                .and_modify(|existing| {
                    if availability_rank(capability.availability)
                        > availability_rank(existing.availability)
                    {
                        existing.availability = capability.availability;
                    }
                })
                .or_insert(capability);
        }
    }
    if merged.is_empty() {
        // No recognized languages: fall back to the tree-sitter floor so the
        // source-served ops (read_file/grep) are still declared.
        merged = capabilities_for_language("");
    }
    merged
}

fn availability_rank(availability: Availability) -> u8 {
    match availability {
        Availability::Unsupported => 0,
        Availability::Partial => 1,
        Availability::Full => 2,
    }
}

/// The portable artifact manifest emitted next to the `.grapha` store.
///
/// Generalizes [`crate::remote::ProjectRevisionMetadata`]: `producer`,
/// `schema_version`, and `source_revision` are the shared, reused fields; the
/// rest are the 0027 additions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub producer: String,
    pub producer_version: String,
    pub schema_version: String,
    /// Recomputable source revision. Today this reuses the git head OID as a
    /// provenance hint; the spec'd `sha256:` source fingerprint binding is
    /// owned by proposal 0026 and consumed by Slice 3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub source_path_mode: SourcePathMode,
    /// Hash of the on-disk cache bytes (the `grapha.db` store + search index).
    pub index_revision: String,
    /// Digest over the index bytes; the signing input in Slice 3.
    pub artifact_digest: String,
    pub languages: Vec<String>,
    /// Per-op two-axis capability matrix, keyed by op name.
    pub capabilities: BTreeMap<String, Capability>,
}

/// Build the manifest from the indexed graph + identity, without touching disk.
///
/// `index_revision` and `artifact_digest` are computed from the store bytes by
/// the caller (see [`emit_manifest`]) because they depend on the written files.
pub fn build_manifest(
    graph: &Graph,
    source_revision: Option<String>,
    index_revision: String,
    artifact_digest: String,
) -> ArtifactManifest {
    let languages = languages_in_graph(graph);
    let capabilities = merge_capabilities(&languages);
    ArtifactManifest {
        producer: PRODUCER.to_string(),
        producer_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: MANIFEST_SCHEMA_VERSION.to_string(),
        source_revision,
        source_path_mode: SourcePathMode::Relative,
        index_revision,
        artifact_digest,
        languages,
        capabilities,
    }
}

/// Emit the unsigned manifest next to the `.grapha` store. Best-effort and
/// additive: it reads the already-written store bytes to compute the digests
/// and writes `manifest.json`. It never mutates the store or the query path.
///
/// Returns the manifest that was written.
pub fn emit_manifest(
    store_dir: &Path,
    graph: &Graph,
    source_revision: Option<String>,
) -> anyhow::Result<ArtifactManifest> {
    let index_revision = hash_store_bytes(store_dir);
    // The artifact digest is, for now, the same content hash of the index
    // bytes; Slice 3 widens this to the full bundle digest that is signed.
    let artifact_digest = index_revision.clone();
    let manifest = build_manifest(graph, source_revision, index_revision, artifact_digest);
    let path = store_dir.join(MANIFEST_FILENAME);
    let payload = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write(&path, payload)?;
    Ok(manifest)
}

/// Collect the distinct languages present in the graph, derived from each
/// node's file extension via the same ids the plugins use.
fn languages_in_graph(graph: &Graph) -> Vec<String> {
    let mut languages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in &graph.nodes {
        if let Some(language) = language_for_path(&node.file) {
            languages.insert(language.to_string());
        }
    }
    languages.into_iter().collect()
}

/// Map a file path to a grapha language id. Mirrors the plugin extension
/// routing (rust_plugin/grapha-swift/polyglot_plugin) so the manifest's
/// language ids line up with the capability tiers.
fn language_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let language = match ext.as_str() {
        "rs" => "rust",
        "swift" => "swift",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "py" | "pyw" => "python",
        "go" => "go",
        "java" => "java",
        "c" => "c",
        "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "rb" | "rake" => "ruby",
        "kt" | "kts" => "kotlin",
        "dart" => "dart",
        "pas" | "dpr" | "dpk" | "lpr" => "pascal",
        _ => return None,
    };
    Some(language)
}

/// A dependency-free content hash of the index bytes (`grapha.db` + the
/// search index directory), expressed `fnv1a64:<hex>`.
///
/// NOTE: this reuses the codebase's existing FNV-1a idiom (`data_paths`) rather
/// than adding a `sha2`/`blake3` dependency. It is sufficient for
/// change-detection (`index_revision`) and an unsigned digest today. The Slice
/// 3 signing spec is expected to upgrade this to a cryptographic `sha256:`
/// digest as the signing input — flagged, not silently chosen.
fn hash_store_bytes(store_dir: &Path) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;

    let hash_file = |path: &Path, hash: &mut u64| {
        if let Ok(bytes) = std::fs::read(path) {
            for byte in bytes {
                *hash ^= u64::from(byte);
                *hash = hash.wrapping_mul(FNV_PRIME);
            }
        }
    };

    hash_file(&store_dir.join("grapha.db"), &mut hash);

    // Fold in the search index directory, in sorted order for determinism.
    let search_dir = store_dir.join("search_index");
    if let Ok(read_dir) = std::fs::read_dir(&search_dir) {
        let mut files: Vec<_> = read_dir
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        for file in files {
            // Mix the relative name in too so renames change the hash.
            if let Some(name) = file.file_name().and_then(|name| name.to_str()) {
                for byte in name.bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(FNV_PRIME);
                }
            }
            hash_file(&file, &mut hash);
        }
    }

    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Graph, Node, NodeKind, Span, Visibility};
    use std::collections::HashMap;

    fn node(file: &str) -> Node {
        Node {
            id: format!("{file}::sym"),
            kind: NodeKind::Function,
            name: "sym".to_string(),
            file: file.into(),
            span: Span {
                start: [1, 0],
                end: [1, 4],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn rust_is_a_deep_two_axis_language() {
        let caps = capabilities_for_language("rust");
        assert_eq!(
            caps["impact"],
            Capability {
                availability: Availability::Full,
                served_by: ServedBy::Graph,
            }
        );
        assert_eq!(
            caps["read_file"],
            Capability {
                availability: Availability::Full,
                served_by: ServedBy::Source,
            }
        );
    }

    #[test]
    fn tree_sitter_language_degrades_deep_ops_but_keeps_source_ops() {
        let caps = capabilities_for_language("python");
        // Deep semantic ops are unsupported on tree-sitter-only languages.
        assert_eq!(caps["impact"].availability, Availability::Unsupported);
        assert_eq!(caps["trace"].availability, Availability::Unsupported);
        assert_eq!(caps["concept"].availability, Availability::Unsupported);
        // Structural ops are partial.
        assert_eq!(caps["search_symbols"].availability, Availability::Partial);
        // Source-served ops stay full and source.
        assert_eq!(caps["grep"].availability, Availability::Full);
        assert_eq!(caps["grep"].served_by, ServedBy::Source);
    }

    #[test]
    fn merge_takes_best_availability_per_op() {
        let merged = merge_capabilities(&["rust".to_string(), "python".to_string()]);
        // Rust supplies a deep impact, so the merged artifact is full.
        assert_eq!(merged["impact"].availability, Availability::Full);
        assert_eq!(merged["impact"].served_by, ServedBy::Graph);
    }

    #[test]
    fn manifest_round_trips_and_declares_languages() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs"), node("web/app.ts")],
            edges: Vec::new(),
        };
        let manifest = build_manifest(
            &graph,
            Some("abc123".to_string()),
            "fnv1a64:0".to_string(),
            "fnv1a64:0".to_string(),
        );
        assert_eq!(manifest.producer, PRODUCER);
        assert_eq!(manifest.schema_version, MANIFEST_SCHEMA_VERSION);
        assert_eq!(manifest.source_path_mode, SourcePathMode::Relative);
        assert_eq!(
            manifest.languages,
            vec!["rust".to_string(), "typescript".to_string()]
        );
        // rust deep ⇒ full impact in the merged matrix.
        assert_eq!(
            manifest.capabilities["impact"].availability,
            Availability::Full
        );

        let json = serde_json::to_string(&manifest).unwrap();
        let restored: ArtifactManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, manifest);
    }

    // ── ADR-0027 Decision 7: per-language capability declaration ──────────
    //
    // The matrix is the source of truth grapha publishes and nous consumes.
    // The two axes (availability × served_by) are orthogonal and never
    // flattened. Swift and Rust are the named "deep" languages; everything
    // else is tree-sitter-only.

    // Acceptance: docs/adr/0027-portable-code-index-artifacts.md Decision 7
    // ("Swift/Rust deep"). Swift must be a deep two-axis language exactly like
    // Rust, since its libIndexStore/SwiftSyntax signal is the motivating case.
    #[test]
    fn test_capabilities_for_language_swift_is_deep() {
        let caps = capabilities_for_language("swift");
        // All deep graph ops are full/graph for a deep language.
        for op in ["search_symbols", "context", "usages", "dependents"] {
            assert_eq!(
                caps[op],
                Capability::graph(Availability::Full),
                "swift {op} should be full/graph"
            );
        }
        // Deep *semantic* ops are also full/graph for a deep language.
        for op in ["concept", "impact", "trace"] {
            assert_eq!(
                caps[op],
                Capability::graph(Availability::Full),
                "swift {op} should be full/graph"
            );
        }
    }

    // Acceptance: docs/adr/0027-portable-code-index-artifacts.md Decision 7.
    // The full two-axis matrix for a deep language, every op pinned, so the
    // contract nous consumes cannot silently drift on a refactor.
    #[test]
    fn test_capabilities_for_language_rust_full_matrix_every_op() {
        let caps = capabilities_for_language("rust");
        let expected: [(&str, Availability, ServedBy); 10] = [
            ("search_symbols", Availability::Full, ServedBy::Graph),
            ("context", Availability::Full, ServedBy::Graph),
            ("usages", Availability::Full, ServedBy::Graph),
            ("dependents", Availability::Full, ServedBy::Graph),
            ("concept", Availability::Full, ServedBy::Graph),
            ("impact", Availability::Full, ServedBy::Graph),
            ("trace", Availability::Full, ServedBy::Graph),
            ("read_file", Availability::Full, ServedBy::Source),
            ("grep", Availability::Full, ServedBy::Source),
            ("codebases", Availability::Full, ServedBy::Graph),
        ];
        assert_eq!(
            caps.len(),
            expected.len(),
            "matrix covers every op exactly once"
        );
        for (op, availability, served_by) in expected {
            assert_eq!(
                caps[op],
                Capability {
                    availability,
                    served_by
                },
                "rust {op} mismatch"
            );
        }
    }

    // Acceptance: docs/adr/0027-portable-code-index-artifacts.md Decision 7
    // ("tree-sitter-only languages mark deeper graph ops partial/unsupported").
    // Full two-axis matrix for a tree-sitter-only language, every op pinned.
    #[test]
    fn test_capabilities_for_language_tree_sitter_full_matrix_every_op() {
        let caps = capabilities_for_language("go");
        let expected: [(&str, Availability, ServedBy); 10] = [
            // Structural graph ops degrade to partial.
            ("search_symbols", Availability::Partial, ServedBy::Graph),
            ("context", Availability::Partial, ServedBy::Graph),
            ("usages", Availability::Partial, ServedBy::Graph),
            ("dependents", Availability::Partial, ServedBy::Graph),
            // Deep semantic graph ops are unsupported.
            ("concept", Availability::Unsupported, ServedBy::Graph),
            ("impact", Availability::Unsupported, ServedBy::Graph),
            ("trace", Availability::Unsupported, ServedBy::Graph),
            // Source-served ops stay full/source regardless of language tier.
            ("read_file", Availability::Full, ServedBy::Source),
            ("grep", Availability::Full, ServedBy::Source),
            // codebases is always full/graph.
            ("codebases", Availability::Full, ServedBy::Graph),
        ];
        assert_eq!(
            caps.len(),
            expected.len(),
            "matrix covers every op exactly once"
        );
        for (op, availability, served_by) in expected {
            assert_eq!(
                caps[op],
                Capability {
                    availability,
                    served_by
                },
                "go {op} mismatch"
            );
        }
    }

    // Acceptance: docs/adr/0027 Decision 5 — `served_by` is a *where*, intrinsic
    // to the op, never collapsed into the availability axis. read_file/grep are
    // always source; codebases is always graph; no language changes that.
    #[test]
    fn test_served_by_is_intrinsic_to_op_across_tiers() {
        for language in ["rust", "swift", "python", "go", "java", "unknown-lang"] {
            let caps = capabilities_for_language(language);
            assert_eq!(
                caps["read_file"].served_by,
                ServedBy::Source,
                "{language} read_file must be served_by source"
            );
            assert_eq!(
                caps["grep"].served_by,
                ServedBy::Source,
                "{language} grep must be served_by source"
            );
            assert_eq!(
                caps["codebases"].served_by,
                ServedBy::Graph,
                "{language} codebases must be served_by graph"
            );
            // Source/codebases ops never degrade by tier.
            assert_eq!(caps["read_file"].availability, Availability::Full);
            assert_eq!(caps["grep"].availability, Availability::Full);
            assert_eq!(caps["codebases"].availability, Availability::Full);
        }
    }

    #[test]
    fn test_language_tier_rust_and_swift_deep_others_tree_sitter() {
        assert_eq!(language_tier("rust"), LanguageTier::Deep);
        assert_eq!(language_tier("swift"), LanguageTier::Deep);
        for language in ["python", "go", "typescript", "java", "", "RUST", "Swift"] {
            // Note: matching is exact/case-sensitive on the plugin id.
            assert_eq!(
                language_tier(language),
                LanguageTier::TreeSitter,
                "{language} should be tree-sitter tier"
            );
        }
    }

    // Acceptance: docs/adr/0027 Decision 7 — merge takes the best (verified)
    // availability per op across the artifact's languages.
    #[test]
    fn test_merge_capabilities_deep_language_lifts_semantic_ops() {
        // Swift (deep) + python (tree-sitter): swift lifts the semantic ops.
        let merged = merge_capabilities(&["python".to_string(), "swift".to_string()]);
        assert_eq!(merged["impact"].availability, Availability::Full);
        assert_eq!(merged["trace"].availability, Availability::Full);
        assert_eq!(merged["concept"].availability, Availability::Full);
        assert_eq!(merged["search_symbols"].availability, Availability::Full);
        // served_by is preserved through the merge.
        assert_eq!(merged["impact"].served_by, ServedBy::Graph);
        assert_eq!(merged["read_file"].served_by, ServedBy::Source);
    }

    #[test]
    fn test_merge_capabilities_all_tree_sitter_stays_degraded() {
        let merged = merge_capabilities(&["python".to_string(), "go".to_string()]);
        // No deep language present: semantic ops stay unsupported.
        assert_eq!(merged["impact"].availability, Availability::Unsupported);
        assert_eq!(merged["search_symbols"].availability, Availability::Partial);
        // Source-served ops still declared so off-box read_file/grep work.
        assert_eq!(merged["read_file"].availability, Availability::Full);
        assert_eq!(merged["read_file"].served_by, ServedBy::Source);
    }

    // The merge falls back to the tree-sitter floor for an empty language set,
    // so read_file/grep are always declared even for a content-less artifact.
    #[test]
    fn test_merge_capabilities_empty_falls_back_to_source_floor() {
        let merged = merge_capabilities(&[]);
        assert!(
            !merged.is_empty(),
            "empty language set must not yield empty matrix"
        );
        assert_eq!(merged["read_file"].availability, Availability::Full);
        assert_eq!(merged["read_file"].served_by, ServedBy::Source);
        assert_eq!(merged["grep"].served_by, ServedBy::Source);
        // No deep language ⇒ deep semantic ops unsupported.
        assert_eq!(merged["impact"].availability, Availability::Unsupported);
    }

    // ── ADR-0027 Decision 7: emitted manifest.json shape ──────────────────

    // Acceptance: docs/adr/0027 Decision 7 — the emitted manifest has the
    // required fields with correct producer/schema_version and a per-op
    // {availability, served_by} matrix. Asserted against the on-disk JSON, not
    // just the in-memory struct, so the wire shape nous reads is pinned.
    #[test]
    fn test_emit_manifest_json_has_required_fields_and_two_axis_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        std::fs::create_dir_all(store_dir.join("search_index")).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"db-bytes").unwrap();

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs"), node("app/View.swift")],
            edges: Vec::new(),
        };
        emit_manifest(&store_dir, &graph, Some("deadbeef".to_string())).unwrap();

        let raw = std::fs::read_to_string(store_dir.join(MANIFEST_FILENAME)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(json["producer"], "grapha");
        assert_eq!(json["schema_version"], MANIFEST_SCHEMA_VERSION);
        assert_eq!(json["producer_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["source_path_mode"], "relative");
        assert_eq!(json["source_revision"], "deadbeef");
        assert!(
            json["index_revision"]
                .as_str()
                .unwrap()
                .starts_with("fnv1a64:"),
            "index_revision must carry an algorithm-tagged digest"
        );
        assert!(json["artifact_digest"].as_str().is_some());

        // languages present and sorted.
        let languages: Vec<String> = serde_json::from_value(json["languages"].clone()).unwrap();
        assert_eq!(languages, vec!["rust".to_string(), "swift".to_string()]);

        // The per-op matrix is two-axis on the wire: {availability, served_by}.
        let caps = &json["capabilities"];
        assert_eq!(caps["impact"]["availability"], "full");
        assert_eq!(caps["impact"]["served_by"], "graph");
        assert_eq!(caps["read_file"]["availability"], "full");
        assert_eq!(caps["read_file"]["served_by"], "source");
        // The flat `source` vocabulary (rejected by Decision 5) must not leak:
        // availability is never the string "source".
        for op in [
            "search_symbols",
            "context",
            "usages",
            "impact",
            "trace",
            "concept",
            "dependents",
            "read_file",
            "grep",
            "codebases",
        ] {
            let availability = caps[op]["availability"].as_str().unwrap();
            assert!(
                matches!(availability, "full" | "partial" | "unsupported"),
                "{op} availability must be a quality, not a where: {availability}"
            );
            assert!(
                caps[op]["served_by"].is_string(),
                "{op} must carry served_by"
            );
        }
    }

    // Acceptance: docs/adr/0027 Decision 5 — `source_revision` is optional
    // provenance only; an artifact with none must still emit a valid manifest.
    #[test]
    fn test_emit_manifest_omits_absent_source_revision() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"db").unwrap();

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs")],
            edges: Vec::new(),
        };
        let manifest = emit_manifest(&store_dir, &graph, None).unwrap();
        assert_eq!(manifest.source_revision, None);

        let raw = std::fs::read_to_string(store_dir.join(MANIFEST_FILENAME)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            json.get("source_revision").is_none(),
            "absent source_revision is skipped on the wire"
        );
    }

    // ── ADR-0027: index_revision/artifact_digest stable for unchanged input ─

    // Acceptance: docs/adr/0027 — "index_revision/artifact_digest are stable
    // for unchanged input." Re-emitting over identical store bytes yields the
    // same digest, so the consumer's change-detection is reliable.
    #[test]
    fn test_index_revision_stable_for_unchanged_store_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        std::fs::create_dir_all(store_dir.join("search_index")).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"stable-store").unwrap();
        std::fs::write(store_dir.join("search_index").join("seg.idx"), b"seg").unwrap();

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs")],
            edges: Vec::new(),
        };

        let first = emit_manifest(&store_dir, &graph, None).unwrap();
        let second = emit_manifest(&store_dir, &graph, None).unwrap();
        assert_eq!(
            first.index_revision, second.index_revision,
            "unchanged store bytes must produce a stable index_revision"
        );
        assert_eq!(first.artifact_digest, second.artifact_digest);
        // Today artifact_digest == index_revision (both content-hash the index).
        assert_eq!(first.artifact_digest, first.index_revision);
    }

    // Acceptance: docs/adr/0027 — the digest must *change* when store bytes
    // change, otherwise "stable for unchanged input" would be vacuous.
    #[test]
    fn test_index_revision_changes_when_store_bytes_change() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        std::fs::create_dir_all(&store_dir).unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs")],
            edges: Vec::new(),
        };

        std::fs::write(store_dir.join("grapha.db"), b"version-one").unwrap();
        let before = emit_manifest(&store_dir, &graph, None).unwrap();

        std::fs::write(store_dir.join("grapha.db"), b"version-two-different").unwrap();
        let after = emit_manifest(&store_dir, &graph, None).unwrap();

        assert_ne!(
            before.index_revision, after.index_revision,
            "changed store bytes must change the index_revision"
        );
    }

    // The digest also covers the search index directory, so a search-index
    // change is detected even when grapha.db is byte-identical.
    #[test]
    fn test_index_revision_covers_search_index_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let search_dir = store_dir.join("search_index");
        std::fs::create_dir_all(&search_dir).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"same-db").unwrap();
        std::fs::write(search_dir.join("a.idx"), b"one").unwrap();

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs")],
            edges: Vec::new(),
        };
        let before = emit_manifest(&store_dir, &graph, None).unwrap();

        std::fs::write(search_dir.join("a.idx"), b"two-changed").unwrap();
        let after = emit_manifest(&store_dir, &graph, None).unwrap();
        assert_ne!(
            before.index_revision, after.index_revision,
            "search-index changes must move the digest"
        );
    }

    // The manifest itself is written into the store dir; re-emitting must not
    // fold the previous manifest.json into the digest (only grapha.db +
    // search_index are hashed), or the digest could never be stable.
    #[test]
    fn test_index_revision_ignores_its_own_manifest_file() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"db").unwrap();
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs")],
            edges: Vec::new(),
        };
        // First emit writes manifest.json; second emit sees it on disk.
        let first = emit_manifest(&store_dir, &graph, None).unwrap();
        assert!(store_dir.join(MANIFEST_FILENAME).is_file());
        let second = emit_manifest(&store_dir, &graph, None).unwrap();
        assert_eq!(
            first.index_revision, second.index_revision,
            "the manifest's own bytes must not feed back into the digest"
        );
    }

    #[test]
    fn emit_manifest_writes_unsigned_json_next_to_store() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        std::fs::create_dir_all(store_dir.join("search_index")).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"store-bytes").unwrap();

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![node("src/lib.rs")],
            edges: Vec::new(),
        };

        let manifest = emit_manifest(&store_dir, &graph, Some("rev".to_string())).unwrap();

        let written = store_dir.join(MANIFEST_FILENAME);
        assert!(written.is_file(), "manifest.json should be emitted");
        assert!(
            !store_dir.join("manifest.sig").exists(),
            "signing is deferred to Slice 3: no manifest.sig is written"
        );
        assert!(manifest.index_revision.starts_with("fnv1a64:"));
        assert_eq!(manifest.artifact_digest, manifest.index_revision);

        let restored: ArtifactManifest =
            serde_json::from_slice(&std::fs::read(written).unwrap()).unwrap();
        assert_eq!(restored, manifest);
    }
}
