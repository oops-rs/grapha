//! Portable code-index artifact manifest (ADR-0027, Slice 3a, producer side).
//!
//! On `grapha index` we additively emit a `manifest.json` next to the `.grapha`
//! store. It generalizes the [`crate::remote`] `ProjectRevisionMetadata`
//! precedent (producer / `schema_version` / `source_revision`) into the
//! two-axis capability manifest a downstream consumer (nous) later ingests.
//!
//! This module is **producer-only and additive**: it never changes how the
//! store is opened or queried, and writing the manifest is best-effort relative
//! to the existing index pipeline.
//!
//! Slice 3 (ADR-0027) upgrades the manifest to **schema v2**: the digest chain
//! is `blake3` end to end (`blake3:<hex>`), and a detached `ed25519` signature
//! over a deterministic canonical JSON body authenticates the bundle. The
//! signing/digest/canonical routines in this module are the **single shared
//! cross-repo contract** — `nous-engine` depends on `grapha-engine` over the
//! git dependency and reuses these `pub` routines verbatim, so the bytes grapha
//! signs are byte-identical to the bytes nous verifies.

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use grapha_core::graph::Graph;

/// The producer name stamped into every grapha-emitted manifest.
pub const PRODUCER: &str = "grapha";

/// Manifest schema version. Manifest fields are additive-only across versions
/// (matching the `remote.rs` `bundle_schema_version` discipline); a consumer
/// allowlists known versions and rejects unknown ones.
///
/// `"2"` is the first transportable bundle schema (signed, blake3 digest chain).
/// A `"1"` manifest is a next-to-`.grapha` producer hint only — not a bundle.
pub const MANIFEST_SCHEMA_VERSION: &str = "2";

/// The filename written next to the `.grapha` store.
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// The detached-signature filename inside a bundle. It is an archive member but
/// is excluded from `artifact_digest` and from the signed canonical body (it
/// cannot be inside what it signs).
pub const SIGNATURE_FILENAME: &str = "manifest.sig";

/// The signature algorithm grapha emits and nous verifies.
pub const SIGNATURE_ALGORITHM: &str = "ed25519";

/// Fixed domain-separation context prefixed to the canonical body before
/// signing/verifying, so a grapha signature can never be cross-protocol
/// replayed against a different message space. **Changing this constant breaks
/// the cross-repo contract** — nous prepends the identical bytes.
pub const SIGNATURE_DOMAIN_SEP: &[u8] = b"grapha:artifact-manifest:v2\0";

/// The digest tag for the blake3 chain (`source_revision` / `artifact_digest`).
pub const BLAKE3_TAG: &str = "blake3:";

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
///
/// DEFERRED (per ADR-0027 Decision 7 / the Slice 3 spec): the artifact-level
/// "best availability" collapse is intentional for now. Per-language capability
/// granularity in the emitted manifest is in-spec but owned by Slice 3; do not
/// add it here as part of the producer-only slices.
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

/// A detached signature descriptor stamped on a signed bundle. The signature
/// bytes themselves live in [`SIGNATURE_FILENAME`]; this names the algorithm,
/// the producer key id that selects the trust-store key, and the sig file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub algorithm: String,
    pub key_id: String,
    pub sig_file: String,
}

/// The portable artifact manifest emitted next to the `.grapha` store and,
/// at schema v2, inside a signed `.nbundle`.
///
/// Generalizes [`crate::remote::ProjectRevisionMetadata`]: `producer`,
/// `schema_version`, and `source_revision` are the shared, reused fields; the
/// rest are the 0027 additions.
///
/// The `signature` field is **excluded** from the canonical signing body (see
/// [`canonical_manifest_bytes`]); every other field — including
/// `artifact_digest` — is a field *inside* the signed body (no fragile
/// `|| digest` append).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub producer: String,
    pub producer_version: String,
    pub schema_version: String,
    /// Recomputable, tagged, full-width source fingerprint (`blake3:<hex>`).
    /// A git commit SHA is at most `source_vcs_hint`, never the identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub source_path_mode: SourcePathMode,
    /// Hash of the on-disk cache bytes (the `grapha.db` store + search index).
    pub index_revision: String,
    /// Tagged `blake3:<hex>` digest over the sorted `index/` bytes; the signed
    /// provenance binding the consumer recomputes and the citation stamps.
    pub artifact_digest: String,
    pub languages: Vec<String>,
    /// Per-op two-axis capability matrix, keyed by op name.
    pub capabilities: BTreeMap<String, Capability>,
    /// The target codebase this bundle is for (anti-cross-targeting binding).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codebase_id: Option<String>,
    /// Producer-asserted freshness window start (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<i64>,
    /// Producer-asserted freshness window end (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Optional VCS provenance hint (e.g. git head OID). Never the identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_vcs_hint: Option<String>,
    /// Present on signed bundles; excluded from the canonical signing body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
}

/// Build the unsigned manifest from the indexed graph + identity, without
/// touching disk. `index_revision`/`artifact_digest` are computed from the
/// store bytes by the caller because they depend on the written files; the
/// `signature` is attached later by [`sign_manifest`] + the bundle writer.
///
/// `source_revision` is the tagged, full-width `blake3:<hex>` source
/// fingerprint (the recomputable identity). The remaining v2 binding fields
/// (`codebase_id`/`issued_at`/`expires_at`/`source_vcs_hint`) are passed
/// through [`ManifestBindings`].
pub fn build_manifest(
    graph: &Graph,
    source_revision: Option<String>,
    index_revision: String,
    artifact_digest: String,
    bindings: ManifestBindings,
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
        codebase_id: bindings.codebase_id,
        issued_at: bindings.issued_at,
        expires_at: bindings.expires_at,
        source_vcs_hint: bindings.source_vcs_hint,
        signature: None,
    }
}

/// The schema-v2 binding fields a producer stamps into the manifest. Kept as a
/// struct so additive bindings do not churn [`build_manifest`]'s arity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestBindings {
    pub codebase_id: Option<String>,
    pub issued_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub source_vcs_hint: Option<String>,
}

/// Emit the unsigned manifest next to the `.grapha` store. Best-effort and
/// additive: it reads the already-written store bytes to compute the digests
/// and writes `manifest.json`. It never mutates the store or the query path.
///
/// Returns the manifest that was written.
pub fn emit_manifest(
    store_dir: &Path,
    graph: &Graph,
    source_vcs_hint: Option<String>,
) -> anyhow::Result<ArtifactManifest> {
    let index_revision = hash_store_bytes(store_dir);
    // `artifact_digest` is the blake3 digest over the index bytes — the signed
    // provenance binding. The next-to-store hint is unsigned; `grapha bundle`
    // (re)computes the digests and signs for transport.
    let artifact_digest = index_revision.clone();
    // The next-to-store hint carries no recomputable source fingerprint (the
    // bundle writer computes it over the materialized `source/` tree); the git
    // head OID, if any, is recorded only as a VCS hint.
    let bindings = ManifestBindings {
        source_vcs_hint,
        ..ManifestBindings::default()
    };
    let manifest = build_manifest(graph, None, index_revision, artifact_digest, bindings);
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
        // `.h` is ambiguous (C or C++); map it to C, the lower common denominator,
        // since both share the tree-sitter capability tier and C is the more
        // conservative attribution for a bare header.
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
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

/// A `blake3` content digest over **exactly the set of files packaged into the
/// bundle's `index/` directory**, expressed `blake3:<64-hex>`.
///
/// This is the **single shared routine** the producer signs over and the
/// consumer (nous) recomputes — both [`crate::bundle::write_bundle`]'s `index/`
/// packaging and `grapha bundle`'s signed `artifact_digest` digest *this* set:
/// a recursive, relative-path-sorted walk of the store directory, mixing each
/// file's relative path (with `/` separators) in before its bytes so a rename or
/// a move to a nested subdir changes the digest. Files at any depth are covered,
/// not just the top level of `search_index/`. The manifest's own next-to-store
/// files (`manifest.json`/`manifest.sig`) are never folded in, matching the
/// bundle packager's exclusion.
///
/// The walk and exclusions are kept in lockstep with
/// [`crate::bundle::store_index_files`] (the bundle packager calls the same
/// collector), so the packaged bytes and the digested bytes can never drift.
pub fn hash_store_bytes(store_dir: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    for (rel, abs) in crate::bundle::store_index_files(store_dir) {
        // Mix the relative path in (with `/` separators) so renames/moves
        // change the digest, then the file bytes.
        hasher.update(rel.as_bytes());
        if let Ok(bytes) = std::fs::read(&abs) {
            hasher.update(&bytes);
        }
    }
    format!("{BLAKE3_TAG}{}", hasher.finalize().to_hex())
}

/// Compute the tagged `blake3:<hex>` digest of an arbitrary byte slice. This is
/// the conformance-vector primitive (fixed bytes -> fixed tagged digest) and
/// the routine nous reuses for any one-shot digest of bundle bytes.
pub fn artifact_digest_blake3(bytes: &[u8]) -> String {
    format!("{BLAKE3_TAG}{}", blake3::hash(bytes).to_hex())
}

// ── Canonical JSON + ed25519 signing (the shared cross-repo contract) ────────

/// Deterministic canonical JSON of the manifest **without** its `signature`
/// field, suitable as the signing/verification body.
///
/// The canonical form is **deterministic, recursive key-sorted, compact JSON**:
/// `serde_json::to_value` → a recursive sort of every object's keys → compact
/// (no-whitespace) serialization, with the `signature` field dropped (it cannot
/// be inside what it signs). It is *not* a general RFC-8785 (JCS) implementation
/// — it does no number canonicalization. That is sufficient here precisely
/// because the manifest contains only `i64` integers and ASCII strings (no
/// floats, no non-string object keys), so `serde_json`'s number formatting is
/// already exact and stable; a [`debug_assert`] below pins that invariant.
///
/// `artifact_digest` is a field *inside* this body, so a forged digest changes
/// the signed bytes and fails verification — there is no fragile `|| digest`
/// append. This is also the **single public routine** nous reuses verbatim over
/// the git dependency (it does not re-mirror a second canonicalizer), so the
/// bytes grapha signs are byte-identical to the bytes nous verifies; the
/// committed conformance vector pins them.
pub fn canonical_manifest_bytes(manifest: &ArtifactManifest) -> Vec<u8> {
    let mut unsigned = manifest.clone();
    unsigned.signature = None;
    // serde_json::Value -> recursive key sort -> compact serialization. The
    // manifest is small, well-typed, and free of floats/non-finite numbers, so
    // this is stable and deterministic across platforms without JCS number
    // canonicalization.
    let value = serde_json::to_value(&unsigned).expect("manifest serializes to JSON");
    debug_assert!(
        !json_has_float(&value),
        "manifest must not contain float values; canonical JSON skips JCS number canonicalization"
    );
    let canonical = canonicalize_json(value);
    serde_json::to_vec(&canonical).expect("canonical JSON serializes")
}

/// Whether any value in the JSON tree is a non-integer (float) number. Used to
/// guard the canonicalization's "integers + ASCII strings only" precondition.
fn json_has_float(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => number.as_i64().is_none() && number.as_u64().is_none(),
        serde_json::Value::Array(items) => items.iter().any(json_has_float),
        serde_json::Value::Object(map) => map.values().any(json_has_float),
        _ => false,
    }
}

/// Recursively sort object keys so serialization is order-independent.
fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonicalize_json).collect())
        }
        other => other,
    }
}

/// Sign the canonical body with the producer signing key, returning the raw
/// 64-byte ed25519 signature written to [`SIGNATURE_FILENAME`]. The signed
/// message is `SIGNATURE_DOMAIN_SEP || canonical_bytes`.
pub fn sign_manifest(canonical_bytes: &[u8], signing_key: &SigningKey) -> Vec<u8> {
    signing_key
        .sign(&domain_separated(canonical_bytes))
        .to_bytes()
        .to_vec()
}

/// Verify a detached ed25519 signature over `SIGNATURE_DOMAIN_SEP ||
/// canonical_bytes`. Returns `false` on any malformed signature or mismatch —
/// never panics, so an attacker-controlled `sig_bytes` cannot crash the gate.
pub fn verify_manifest(
    canonical_bytes: &[u8],
    sig_bytes: &[u8],
    verifying_key: &VerifyingKey,
) -> bool {
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes) else {
        return false;
    };
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
    verifying_key
        .verify(&domain_separated(canonical_bytes), &signature)
        .is_ok()
}

fn domain_separated(canonical_bytes: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN_SEP.len() + canonical_bytes.len());
    message.extend_from_slice(SIGNATURE_DOMAIN_SEP);
    message.extend_from_slice(canonical_bytes);
    message
}

/// Load an ed25519 signing key from a 32-byte seed file (raw bytes). This is
/// the key format `grapha bundle --sign-key` accepts; the producer key id is
/// derived from the verifying key. Pure-Rust; no OpenSSL/PEM machinery.
pub fn load_signing_key(seed: &[u8]) -> anyhow::Result<SigningKey> {
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519 signing key seed must be exactly 32 bytes"))?;
    Ok(SigningKey::from_bytes(&seed))
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
            Some("blake3:abc123".to_string()),
            "blake3:0".to_string(),
            "blake3:0".to_string(),
            ManifestBindings::default(),
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
        // The git head OID is a VCS hint, never the recomputable identity.
        assert_eq!(json["source_vcs_hint"], "deadbeef");
        assert!(
            json.get("source_revision").is_none(),
            "the next-to-store hint carries no recomputable source fingerprint"
        );
        assert!(
            json["index_revision"]
                .as_str()
                .unwrap()
                .starts_with("blake3:"),
            "index_revision must carry the blake3 algorithm tag"
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

    // LOW-1 fix: the digest covers files at ANY depth under the store, so a
    // file in a *subdirectory* of search_index/ moves the digest. Without the
    // recursive walk such a nested file would be packaged-but-not-digested.
    #[test]
    fn test_index_revision_covers_nested_store_file() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let nested = store_dir.join("search_index").join("segments").join("part");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(store_dir.join("grapha.db"), b"same-db").unwrap();
        std::fs::write(nested.join("data.idx"), b"v1").unwrap();

        let before = hash_store_bytes(&store_dir);

        // Mutating a nested file must change the digest.
        std::fs::write(nested.join("data.idx"), b"v2-changed").unwrap();
        let after = hash_store_bytes(&store_dir);
        assert_ne!(
            before, after,
            "a change to a file nested under search_index/ must move the digest"
        );

        // Adding a brand-new nested file must also change the digest.
        std::fs::write(nested.join("more.idx"), b"new").unwrap();
        let after_add = hash_store_bytes(&store_dir);
        assert_ne!(
            after, after_add,
            "a new nested store file must move the digest (packaged ⇒ digested)"
        );
    }

    // The manifest itself is written into the store dir; re-emitting must not
    // fold the previous manifest.json into the digest (only the store files are
    // hashed), or the digest could never be stable.
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
            !store_dir.join(SIGNATURE_FILENAME).exists(),
            "the next-to-store hint is unsigned: no manifest.sig is written here"
        );
        assert!(manifest.index_revision.starts_with(BLAKE3_TAG));
        assert_eq!(manifest.artifact_digest, manifest.index_revision);

        let restored: ArtifactManifest =
            serde_json::from_slice(&std::fs::read(written).unwrap()).unwrap();
        assert_eq!(restored, manifest);
    }

    // ── ADR-0027 §0 + §cross-repo: the shared signing/digest contract ─────────
    //
    // These are the conformance vectors nous asserts against across the git
    // dependency. The expected values are explicit constants so a drift in the
    // canonicalization, digest, or domain-sep on either side fails loudly.

    fn hex_lower(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A fully-pinned schema-v2 manifest used to anchor canonical bytes +
    /// signature. Every field is fixed so the canonical body is deterministic.
    fn fixed_vector_manifest() -> ArtifactManifest {
        let mut capabilities = BTreeMap::new();
        capabilities.insert(
            "search_symbols".to_string(),
            Capability::graph(Availability::Full),
        );
        capabilities.insert(
            "read_file".to_string(),
            Capability::source(Availability::Full),
        );
        ArtifactManifest {
            producer: "grapha".to_string(),
            producer_version: "0.0.0-vector".to_string(),
            schema_version: "2".to_string(),
            source_revision: Some("blake3:00ff".to_string()),
            source_path_mode: SourcePathMode::Relative,
            index_revision: "blake3:1234".to_string(),
            artifact_digest: "blake3:abcd".to_string(),
            languages: vec!["rust".to_string(), "swift".to_string()],
            capabilities,
            codebase_id: Some("demo-codebase".to_string()),
            issued_at: Some(1_700_000_000),
            expires_at: Some(1_800_000_000),
            source_vcs_hint: Some("gitsha".to_string()),
            signature: None,
        }
    }

    // M2 guard: the canonical body relies on the manifest carrying only
    // integers + ASCII strings (no floats), so serde_json number formatting is
    // already exact and JCS number canonicalization is unnecessary. Assert a
    // realistic manifest has no float values.
    #[test]
    fn canonical_manifest_has_no_float_values() {
        let manifest = fixed_vector_manifest();
        let value = serde_json::to_value(&manifest).unwrap();
        assert!(
            !json_has_float(&value),
            "the manifest must not contain float values"
        );
        // The detector itself must flag a float (so the guard is not vacuous).
        let with_float = serde_json::json!({ "n": 1.5 });
        assert!(json_has_float(&with_float));
        // ...and must NOT flag plain integers.
        let only_ints = serde_json::json!({ "a": 1, "b": [2, 3], "c": -4 });
        assert!(!json_has_float(&only_ints));
    }

    // Conformance vector 1: fixed input bytes -> fixed tagged blake3 digest.
    #[test]
    fn conformance_blake3_digest_of_fixed_bytes() {
        let digest = artifact_digest_blake3(b"grapha-conformance-vector");
        assert_eq!(
            digest, "blake3:cedfd1155eb3c18b461c601f3c96d332c78c668dcb24a5e92a68290385bff7cf",
            "fixed bytes must map to a fixed tagged blake3 digest"
        );
    }

    // Conformance vector 1b: a FIXED store layout -> fixed tagged digest from
    // the shared `hash_store_bytes` routine. This pins the *recursive* store
    // walk (M1/LOW-1): `grapha.db`, a top-level search file, and a file nested
    // two levels deep, each mixed in as `<relative-path-with-slashes><bytes>` in
    // relative-path sort order. nous recomputes this exact value over a bundle's
    // `index/`. (Changed vs 6ba47a9, which only digested the top level of
    // search_index/ and mixed the bare file name, not the full relative path.)
    #[test]
    fn conformance_hash_store_bytes_over_fixed_recursive_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join(".grapha");
        let nested = store.join("search_index").join("segments").join("000");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(store.join("grapha.db"), b"db-bytes").unwrap();
        std::fs::write(store.join("search_index").join("seg.idx"), b"top-seg").unwrap();
        std::fs::write(nested.join("part.idx"), b"nested-bytes").unwrap();

        let digest = hash_store_bytes(&store);
        assert_eq!(
            digest, "blake3:723be7ee4b0a62075a7b4a877eff2ef2889da52ab403541be088f7f879d253ea",
            "fixed recursive store layout must map to a fixed tagged digest"
        );
    }

    // Conformance vector 2: a fixed manifest -> fixed canonical bytes. The
    // canonical form sorts keys and drops the signature field.
    #[test]
    fn conformance_canonical_bytes_are_sorted_and_drop_signature() {
        let manifest = fixed_vector_manifest();
        let canonical = canonical_manifest_bytes(&manifest);
        let text = String::from_utf8(canonical.clone()).unwrap();

        // Signature excluded; keys sorted (artifact_digest before producer).
        assert!(
            !text.contains("\"signature\""),
            "signature excluded from body"
        );
        assert!(text.starts_with("{\"artifact_digest\":\"blake3:abcd\","));
        let digest_pos = text.find("artifact_digest").unwrap();
        let producer_pos = text.find("\"producer\"").unwrap();
        assert!(digest_pos < producer_pos, "object keys are sorted");

        // Attaching a signature must NOT change the canonical body.
        let mut signed = manifest.clone();
        signed.signature = Some(Signature {
            algorithm: SIGNATURE_ALGORITHM.to_string(),
            key_id: "k1".to_string(),
            sig_file: SIGNATURE_FILENAME.to_string(),
        });
        assert_eq!(canonical, canonical_manifest_bytes(&signed));
    }

    // Conformance vector 3: a FIXED seed -> fixed keypair -> fixed signature
    // that verifies; a tampered manifest is rejected.
    #[test]
    fn conformance_sign_verify_roundtrip_and_tamper_reject() {
        // Fixed 32-byte seed -> deterministic ed25519 keypair.
        let seed: [u8; 32] = [7u8; 32];
        let signing_key = load_signing_key(&seed).unwrap();
        let verifying_key = signing_key.verifying_key();

        let manifest = fixed_vector_manifest();
        let canonical = canonical_manifest_bytes(&manifest);
        let sig = sign_manifest(&canonical, &signing_key);

        // ed25519 over a fixed seed + fixed message is deterministic: signing
        // twice yields byte-identical signatures, pinned to an explicit hex
        // constant so nous can assert against the identical bytes.
        const EXPECTED_SIG_HEX: &str = "463ce037f273f36f5b60eca7c0f36e27caf0f9fd3d1861af4eddc82a3e0447b0c90b049cfd78eaa23b996f0e7c0f8e3e00fa7e13d284c216a72e7b83a3427f0d";
        assert_eq!(
            hex_lower(&sig),
            EXPECTED_SIG_HEX,
            "fixed seed + fixed body -> fixed signature"
        );
        assert_eq!(sig, sign_manifest(&canonical, &signing_key));
        assert_eq!(sig.len(), 64);
        assert!(verify_manifest(&canonical, &sig, &verifying_key));

        // Tamper: flip one byte of the digest -> different canonical body ->
        // signature no longer verifies.
        let mut tampered = manifest.clone();
        tampered.artifact_digest = "blake3:abce".to_string();
        let tampered_canonical = canonical_manifest_bytes(&tampered);
        assert_ne!(canonical, tampered_canonical);
        assert!(!verify_manifest(&tampered_canonical, &sig, &verifying_key));

        // A malformed (wrong-length) signature is rejected, not panicked on.
        assert!(!verify_manifest(&canonical, b"too-short", &verifying_key));
    }
}
