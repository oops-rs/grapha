//! `grapha bundle` — build a portable, signed `.nbundle` artifact (ADR-0027,
//! Slice 3 producer side).
//!
//! It indexes the project if needed, computes the blake3 digests over the
//! `.grapha` store and the source tree, builds the schema-v2 manifest, signs
//! the canonical body with the operator's ed25519 key, and writes the
//! deterministic `.nbundle`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;

use crate::bundle::{source_fingerprint, write_bundle};
use crate::manifest::{self, ManifestBindings, SIGNATURE_ALGORITHM, SIGNATURE_FILENAME, Signature};

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_bundle(
    source: PathBuf,
    store: Option<PathBuf>,
    out: PathBuf,
    sign_key: PathBuf,
    codebase_id: Option<String>,
    valid_for_secs: i64,
    verbose: bool,
) -> anyhow::Result<()> {
    let store_dir = store.unwrap_or_else(|| source.join(".grapha"));

    // Index first if the store is missing, so `grapha bundle` is usable on a
    // fresh checkout without a separate `grapha index` step.
    if !store_dir.join("grapha.db").is_file() {
        if verbose {
            eprintln!("  building index (no store at {})...", store_dir.display());
        }
        crate::app::index::handle_index(
            source.clone(),
            "sqlite".to_string(),
            Some(store_dir.clone()),
            false,
            false,
        )?;
    }

    let graph = crate::app::index::load_graph(&source).context("loading graph for bundle")?;

    // Digests: blake3 over the index bytes; blake3 over the source tree. The
    // signed `artifact_digest` is the SAME shared `hash_store_bytes` routine the
    // consumer recomputes — never a private re-implementation of its ordering,
    // so grapha can never sign a digest nous cannot reproduce.
    let index_revision = manifest::hash_store_bytes(&store_dir);
    let artifact_digest = index_revision.clone();
    let revision = source_fingerprint(&source).context("fingerprinting source tree")?;

    // Freshness window + provenance hint.
    let issued_at = current_unix_secs();
    let expires_at = issued_at + valid_for_secs;
    let source_vcs_hint = crate::data_paths::project_identity(&source).head_oid;

    let bindings = ManifestBindings {
        codebase_id,
        issued_at: Some(issued_at),
        expires_at: Some(expires_at),
        source_vcs_hint,
    };
    let mut manifest = manifest::build_manifest(
        &graph,
        Some(revision),
        index_revision,
        artifact_digest,
        bindings,
    );

    // Sign the canonical body, then stamp the signature descriptor.
    let signing_key = load_key(&sign_key)?;
    let key_id = key_id_for(&signing_key);
    let canonical = manifest::canonical_manifest_bytes(&manifest);
    let sig = manifest::sign_manifest(&canonical, &signing_key);
    manifest.signature = Some(Signature {
        algorithm: SIGNATURE_ALGORITHM.to_string(),
        key_id: key_id.clone(),
        sig_file: SIGNATURE_FILENAME.to_string(),
    });

    // Re-derive the canonical body AFTER attaching the descriptor to confirm it
    // is byte-identical (the signature field is excluded from the body).
    debug_assert_eq!(canonical, manifest::canonical_manifest_bytes(&manifest));

    write_bundle(&source, &store_dir, &manifest, &sig, &out).context("writing .nbundle")?;

    if verbose {
        eprintln!(
            "  \x1b[32m✓\x1b[0m wrote {} (producer key {}, {} languages, digest {})",
            out.display(),
            key_id,
            manifest.languages.len(),
            manifest.artifact_digest
        );
    } else {
        println!("{}", out.display());
    }
    Ok(())
}

fn load_key(path: &Path) -> anyhow::Result<ed25519_dalek::SigningKey> {
    let seed =
        std::fs::read(path).with_context(|| format!("reading signing key {}", path.display()))?;
    manifest::load_signing_key(&seed)
}

/// Derive a stable producer key id from the verifying (public) key bytes:
/// the leading bytes of the blake3 digest of the public key, hex-encoded. This
/// gives the trust store a stable selector without distributing a separate id.
fn key_id_for(signing_key: &ed25519_dalek::SigningKey) -> String {
    let public = signing_key.verifying_key().to_bytes();
    // `artifact_digest_blake3` returns "blake3:<64hex>"; take the hex tail.
    let tagged = manifest::artifact_digest_blake3(&public);
    let hex = tagged.strip_prefix("blake3:").unwrap_or(&tagged);
    format!("ed25519:{}", &hex[..16])
}

fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
