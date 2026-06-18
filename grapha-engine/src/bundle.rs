//! Deterministic portable code-index bundle (`.nbundle`) writer (ADR-0027,
//! Slice 3, producer side).
//!
//! A bundle is an **uncompressed deterministic tar** containing
//! `{ manifest.json, manifest.sig, source/<tree>, index/<.grapha store> }`. It
//! is the digest substrate: two runs over one revision yield a byte-identical
//! archive and one `artifact_digest`.
//!
//! Determinism rules (see the spec "Packaging and transport"):
//! - entries sorted by relative path (the same ordering `hash_store_bytes` and
//!   the gitignore-aware source walk use);
//! - entry metadata normalized: mtime 0, mode 0644 files / 0755 dirs,
//!   uid/gid/uname/gname zeroed;
//! - `manifest.sig` is an archive member but is **excluded** from
//!   `artifact_digest` and from the signed canonical body;
//! - `manifest.json` is staged **last**, after `source/` and `index/`.
//!
//! The writer never reads absolute host paths into the archive — every member
//! path is relative.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use ignore::WalkBuilder;

use crate::manifest::{ArtifactManifest, MANIFEST_FILENAME, SIGNATURE_FILENAME};

/// Directory names excluded from the packaged `source/` tree (in addition to
/// the gitignore-aware walk's own filtering).
const EXCLUDED_DIRS: [&str; 3] = [".git", "target", ".grapha"];

/// A staged archive member: its relative path inside the bundle and the host
/// file it is read from. Sorted by `rel_path` before writing for determinism.
struct Member {
    rel_path: String,
    abs_path: PathBuf,
}

/// Write a deterministic uncompressed `.nbundle` tar to `out_path`.
///
/// - `source_root` is the gitignore-filtered source tree, packaged under
///   `source/` with relative paths (excludes `.git`/`target`/`.grapha`).
/// - `store_dir` is the on-disk `.grapha` store, packaged under `index/`.
/// - `manifest` + `sig` are written as `manifest.json` (last) and `manifest.sig`.
///
/// Repeated calls over the same inputs produce byte-identical archives.
pub fn write_bundle(
    source_root: &Path,
    store_dir: &Path,
    manifest: &ArtifactManifest,
    sig: &[u8],
    out_path: &Path,
) -> anyhow::Result<()> {
    let mut members = Vec::new();
    collect_source_members(source_root, &mut members)?;
    collect_index_members(store_dir, &mut members);
    // Sort by relative path so member ordering is deterministic regardless of
    // filesystem walk order.
    members.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating bundle parent dir {}", parent.display()))?;
    }

    let file = std::fs::File::create(out_path)
        .with_context(|| format!("creating bundle {}", out_path.display()))?;
    let mut builder = tar::Builder::new(file);
    // Treat large files explicitly; we only ever write regular files + bytes.
    builder.mode(tar::HeaderMode::Deterministic);

    // 1) source/ + index/ entries, sorted.
    for member in &members {
        let bytes = std::fs::read(&member.abs_path)
            .with_context(|| format!("reading bundle member {}", member.abs_path.display()))?;
        append_regular_file(&mut builder, &member.rel_path, &bytes)?;
    }

    // 2) manifest.sig — an archive member, excluded from the digest/signed body.
    append_regular_file(&mut builder, SIGNATURE_FILENAME, sig)?;

    // 3) manifest.json LAST (its digests depend on the staged index bytes).
    let manifest_bytes = serde_json::to_vec(manifest).context("serializing manifest.json")?;
    append_regular_file(&mut builder, MANIFEST_FILENAME, &manifest_bytes)?;

    builder
        .into_inner()
        .context("finalizing bundle tar")?
        .flush()?;
    Ok(())
}

/// Compute the tagged, full-width `blake3:<hex>` source fingerprint of a tree.
///
/// This is the recomputable `source_revision` identity: a gitignore-aware,
/// sorted, per-file content hash that excludes `.git`/`target`/`.grapha`. The
/// fold mixes each file's relative path (with `/` separators) before its bytes
/// so a rename changes the fingerprint. It is the **shared cross-repo routine**
/// the consumer recomputes over the materialized `source/` tree to verify the
/// asserted revision; the two must walk identical bytes in identical order.
pub fn source_fingerprint(source_root: &Path) -> anyhow::Result<String> {
    let mut members = Vec::new();
    collect_source_members(source_root, &mut members)?;
    // Strip the `source/` prefix so the fingerprint is over the tree itself,
    // independent of the bundle's archive layout, and sort for determinism.
    let mut rels: Vec<(String, PathBuf)> = members
        .into_iter()
        .map(|m| {
            let rel = m
                .rel_path
                .strip_prefix("source/")
                .unwrap_or(&m.rel_path)
                .to_string();
            (rel, m.abs_path)
        })
        .collect();
    rels.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = blake3::Hasher::new();
    for (rel, abs) in rels {
        hasher.update(rel.as_bytes());
        hasher.update(&[0u8]); // length-independent separator
        let bytes =
            std::fs::read(&abs).with_context(|| format!("fingerprinting {}", abs.display()))?;
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(format!(
        "{}{}",
        crate::manifest::BLAKE3_TAG,
        hasher.finalize().to_hex()
    ))
}

/// Append a single regular file with fully-normalized, deterministic metadata.
fn append_regular_file<W: Write>(
    builder: &mut tar::Builder<W>,
    rel_path: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    // Zero the owner/group names so the archive carries no host identity.
    header.set_username("").context("clearing tar username")?;
    header.set_groupname("").context("clearing tar groupname")?;
    header.set_cksum();
    builder
        .append_data(&mut header, rel_path, bytes)
        .with_context(|| format!("appending {rel_path} to bundle"))?;
    Ok(())
}

/// Collect the gitignore-aware, relative `source/` tree, excluding the dirs the
/// `source_walk` discipline drops. Files only (symlinks/dirs are not packaged
/// as entries; directories are implied by their members' relative paths).
fn collect_source_members(source_root: &Path, out: &mut Vec<Member>) -> anyhow::Result<()> {
    if !source_root.exists() {
        return Ok(());
    }
    let walker = WalkBuilder::new(source_root)
        .hidden(false)
        .git_ignore(true)
        .build();
    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        if path == source_root {
            continue;
        }
        let rel = path
            .strip_prefix(source_root)
            .with_context(|| format!("relativizing {}", path.display()))?;
        if has_excluded_component(rel) {
            continue;
        }
        // Package regular files only; symlinks are never written (a bundle
        // carries regular files, the gate rejects symlinks on the consumer).
        let file_type = entry.file_type();
        let is_file = file_type.map(|t| t.is_file()).unwrap_or(false);
        let is_symlink = file_type.map(|t| t.is_symlink()).unwrap_or(false);
        if !is_file || is_symlink {
            continue;
        }
        out.push(Member {
            rel_path: join_rel("source", rel),
            abs_path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Collect the `.grapha` store under `index/`: every store file (at any depth),
/// in the same relative shape the consumer opens. Delegates the walk to the
/// shared [`store_index_files`] so the packaged set is byte-for-byte the set
/// `crate::manifest::hash_store_bytes` digests.
fn collect_index_members(store_dir: &Path, out: &mut Vec<Member>) {
    for (rel, abs) in store_index_files(store_dir) {
        out.push(Member {
            rel_path: format!("index/{rel}"),
            abs_path: abs,
        });
    }
}

/// Enumerate the store files that get packaged into a bundle's `index/`, as
/// `(relative-path, absolute-path)` pairs sorted by relative path.
///
/// This is the **single source of truth** for *which* files form the index
/// substrate: a recursive walk of `store_dir` covering regular files at any
/// depth, with `/`-separated relative paths, excluding the next-to-store
/// `manifest.json`/`manifest.sig` hints and symlinks. Both the bundle packager
/// ([`collect_index_members`]) and the digest ([`crate::manifest::hash_store_bytes`])
/// iterate this exact set, so a file is never packaged-but-not-digested (or the
/// reverse). On a missing store or any unreadable path, it yields what it can.
pub fn store_index_files(store_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if !store_dir.exists() {
        return out;
    }
    let walker = WalkBuilder::new(store_dir)
        .hidden(false)
        .git_ignore(false)
        .standard_filters(false)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path == store_dir {
            continue;
        }
        let Ok(rel) = path.strip_prefix(store_dir) else {
            continue;
        };
        // Never package/digest the next-to-store manifest hint.
        if rel == Path::new(MANIFEST_FILENAME) || rel == Path::new(SIGNATURE_FILENAME) {
            continue;
        }
        let file_type = entry.file_type();
        let is_file = file_type.map(|t| t.is_file()).unwrap_or(false);
        let is_symlink = file_type.map(|t| t.is_symlink()).unwrap_or(false);
        if !is_file || is_symlink {
            continue;
        }
        out.push((rel_with_slashes(rel), path.to_path_buf()));
    }
    // Sort by relative path so both packaging order and digest order are
    // deterministic regardless of filesystem walk order.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Join a bundle prefix (`source` / `index`) with a relative path, using `/`
/// separators so the archive paths are platform-independent.
fn join_rel(prefix: &str, rel: &Path) -> String {
    format!("{prefix}/{}", rel_with_slashes(rel))
}

/// Render a relative path with `/` separators so the result is identical across
/// platforms (the digest and the archive both depend on this normalization).
fn rel_with_slashes(rel: &Path) -> String {
    rel.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn has_excluded_component(rel: &Path) -> bool {
    rel.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        EXCLUDED_DIRS.contains(&name.as_ref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        ManifestBindings, artifact_digest_blake3, build_manifest, canonical_manifest_bytes,
        hash_store_bytes, load_signing_key, sign_manifest,
    };
    use grapha_core::graph::Graph;
    use std::collections::BTreeSet;
    use std::io::Read;

    fn empty_graph() -> Graph {
        Graph {
            version: "0.1.0".to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Build a tiny source tree + .grapha store under `root`.
    fn scaffold(root: &Path) -> (PathBuf, PathBuf) {
        let source = root.join("source-root");
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::write(source.join("src").join("lib.rs"), b"fn a() {}").unwrap();
        std::fs::write(source.join("README.md"), b"# demo").unwrap();
        // Excluded dirs that must NOT be packaged.
        std::fs::create_dir_all(source.join("target")).unwrap();
        std::fs::write(source.join("target").join("junk.o"), b"obj").unwrap();
        std::fs::create_dir_all(source.join(".git")).unwrap();
        std::fs::write(source.join(".git").join("HEAD"), b"ref").unwrap();

        let store = root.join("store");
        std::fs::create_dir_all(store.join("search_index")).unwrap();
        std::fs::write(store.join("grapha.db"), b"db-bytes").unwrap();
        std::fs::write(store.join("search_index").join("seg.idx"), b"seg").unwrap();
        (source, store)
    }

    fn make_manifest(store: &Path) -> ArtifactManifest {
        let index_revision = hash_store_bytes(store);
        let artifact_digest = index_revision.clone();
        build_manifest(
            &empty_graph(),
            Some("blake3:00".to_string()),
            index_revision,
            artifact_digest,
            ManifestBindings::default(),
        )
    }

    fn entry_names(bundle: &Path) -> Vec<String> {
        let file = std::fs::File::open(bundle).unwrap();
        let mut archive = tar::Archive::new(file);
        archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn bundle_contains_required_members_and_no_absolute_or_traversal_paths() {
        let dir = tempfile::tempdir().unwrap();
        let (source, store) = scaffold(dir.path());
        let manifest = make_manifest(&store);
        let out = dir.path().join("artifact.nbundle");
        write_bundle(&source, &store, &manifest, b"sig-bytes", &out).unwrap();

        let names = entry_names(&out);
        let set: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        assert!(set.contains("manifest.json"));
        assert!(set.contains("manifest.sig"));
        assert!(set.contains("source/src/lib.rs"));
        assert!(set.contains("source/README.md"));
        assert!(set.contains("index/grapha.db"));
        assert!(set.contains("index/search_index/seg.idx"));

        // Excluded dirs never packaged.
        assert!(!names.iter().any(|n| n.contains("target/")));
        assert!(!names.iter().any(|n| n.contains(".git/")));

        // No absolute path, no traversal.
        for name in &names {
            assert!(!name.starts_with('/'), "absolute path leaked: {name}");
            assert!(!name.contains(".."), "traversal leaked: {name}");
        }
    }

    #[test]
    fn manifest_json_is_written_last() {
        let dir = tempfile::tempdir().unwrap();
        let (source, store) = scaffold(dir.path());
        let manifest = make_manifest(&store);
        let out = dir.path().join("artifact.nbundle");
        write_bundle(&source, &store, &manifest, b"sig", &out).unwrap();

        let names = entry_names(&out);
        assert_eq!(
            names.last().map(String::as_str),
            Some("manifest.json"),
            "manifest.json must be the final archive member"
        );
    }

    #[test]
    fn two_runs_over_one_revision_are_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let (source, store) = scaffold(dir.path());
        let manifest = make_manifest(&store);

        let out1 = dir.path().join("a.nbundle");
        let out2 = dir.path().join("b.nbundle");
        write_bundle(&source, &store, &manifest, b"sig", &out1).unwrap();
        write_bundle(&source, &store, &manifest, b"sig", &out2).unwrap();

        let bytes1 = std::fs::read(&out1).unwrap();
        let bytes2 = std::fs::read(&out2).unwrap();
        assert_eq!(bytes1, bytes2, "deterministic tar must be byte-identical");

        // One artifact_digest over the whole archive.
        assert_eq!(
            artifact_digest_blake3(&bytes1),
            artifact_digest_blake3(&bytes2)
        );
    }

    #[test]
    fn members_are_sorted_by_relative_path_with_manifest_files_after() {
        let dir = tempfile::tempdir().unwrap();
        let (source, store) = scaffold(dir.path());
        let manifest = make_manifest(&store);
        let out = dir.path().join("artifact.nbundle");
        write_bundle(&source, &store, &manifest, b"sig", &out).unwrap();

        let names = entry_names(&out);
        // The source/index members come first, in sorted order; the two
        // manifest files come last (sig then json).
        let split = names.len() - 2;
        let body = &names[..split];
        let mut sorted = body.to_vec();
        sorted.sort();
        assert_eq!(body, sorted.as_slice(), "body members are sorted");
        assert_eq!(&names[split..], &["manifest.sig", "manifest.json"]);
    }

    #[test]
    fn source_fingerprint_is_deterministic_and_excludes_dropped_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let (source, _) = scaffold(dir.path());

        let first = source_fingerprint(&source).unwrap();
        let second = source_fingerprint(&source).unwrap();
        assert_eq!(first, second, "fingerprint is deterministic");
        assert!(first.starts_with("blake3:"));

        // Adding junk under an excluded dir does not move the fingerprint.
        std::fs::write(source.join("target").join("more.o"), b"x").unwrap();
        assert_eq!(source_fingerprint(&source).unwrap(), first);

        // Changing a real source file does move it.
        std::fs::write(source.join("README.md"), b"# changed").unwrap();
        assert_ne!(source_fingerprint(&source).unwrap(), first);
    }

    /// Scaffold a store with a top-level db + a top-level search file + a file
    /// nested two levels deep under search_index/.
    fn scaffold_nested_store(root: &Path) -> PathBuf {
        let store = root.join("store");
        let nested = store.join("search_index").join("segments").join("000");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(store.join("grapha.db"), b"db-bytes").unwrap();
        std::fs::write(store.join("search_index").join("seg.idx"), b"top-seg").unwrap();
        std::fs::write(nested.join("part.idx"), b"nested-bytes").unwrap();
        store
    }

    // M1 + LOW-1: the set of files packaged into `index/` must be EXACTLY the
    // set `hash_store_bytes` digests — same files, including nested ones — so a
    // store file can never be packaged-but-not-digested. Both call the shared
    // `store_index_files` collector.
    #[test]
    fn packaged_index_members_match_hashed_store_files_including_nested() {
        let dir = tempfile::tempdir().unwrap();
        let store = scaffold_nested_store(dir.path());

        // The packaged `index/` member paths.
        let mut members = Vec::new();
        collect_index_members(&store, &mut members);
        let packaged: BTreeSet<String> = members.iter().map(|m| m.rel_path.clone()).collect();

        // The digested store files (relative-to-store), prefixed to `index/`.
        let digested: BTreeSet<String> = store_index_files(&store)
            .into_iter()
            .map(|(rel, _)| format!("index/{rel}"))
            .collect();

        assert_eq!(
            packaged, digested,
            "packaged and digested store-file sets must be identical"
        );
        // The nested file is present in BOTH sets.
        assert!(packaged.contains("index/search_index/segments/000/part.idx"));
        assert!(digested.contains("index/search_index/segments/000/part.idx"));
    }

    // The shared digest agrees end to end over a multi-file + nested store, and
    // moving a nested file's bytes moves the digest (it is genuinely covered).
    #[test]
    fn hash_store_bytes_covers_nested_file_under_search_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = scaffold_nested_store(dir.path());

        let before = hash_store_bytes(&store);
        // Same routine over the same bytes is stable.
        assert_eq!(before, hash_store_bytes(&store));

        // Change ONLY the deeply-nested file; the digest must move.
        std::fs::write(
            store
                .join("search_index")
                .join("segments")
                .join("000")
                .join("part.idx"),
            b"nested-bytes-changed",
        )
        .unwrap();
        assert_ne!(
            before,
            hash_store_bytes(&store),
            "a nested store-file change must move the shared digest"
        );
    }

    #[test]
    fn packaged_manifest_round_trips_and_signature_verifies_against_index_digest() {
        let dir = tempfile::tempdir().unwrap();
        let (source, store) = scaffold(dir.path());

        // Build a signed manifest the way `grapha bundle` does.
        let manifest = make_manifest(&store);
        let signing_key = load_signing_key(&[3u8; 32]).unwrap();
        let canonical = canonical_manifest_bytes(&manifest);
        let sig = sign_manifest(&canonical, &signing_key);

        let out = dir.path().join("artifact.nbundle");
        write_bundle(&source, &store, &manifest, &sig, &out).unwrap();

        // Read manifest.json back out of the archive and reverify.
        let file = std::fs::File::open(&out).unwrap();
        let mut archive = tar::Archive::new(file);
        let mut found = None;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().to_string_lossy() == MANIFEST_FILENAME {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).unwrap();
                found = Some(buf);
            }
        }
        let restored: ArtifactManifest =
            serde_json::from_slice(&found.expect("manifest.json present")).unwrap();
        assert_eq!(restored, manifest);
        assert!(crate::manifest::verify_manifest(
            &canonical_manifest_bytes(&restored),
            &sig,
            &signing_key.verifying_key()
        ));
    }
}
