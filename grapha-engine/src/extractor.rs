//! Ecosystem-neutral dependency-manifest extraction seam (ADR-0069 Decision 2,
//! `docs/adr/0069-grounded-dependency-facts.md` in the `nous` sibling repo).
//!
//! `ManifestDependencyExtractor` factors the two mechanical stages every
//! manifest-format dependency extractor needs — find the manifest files under
//! a project root, then read each manifest's declared dependencies — behind a
//! small trait ecosystem implementations can share.
//! [`cargo_manifest::CargoDependencyExtractor`](crate::cargo_manifest::CargoDependencyExtractor)
//! is the first implementation; `package.json` (npm/yarn/pnpm workspaces) and
//! `pyproject.toml` (`[project.dependencies]`, Poetry, uv) can follow later as
//! siblings behind this same seam, without touching `query::deps`,
//! `dependents_hits`, or the `code_dependents` tool contract on the Nous side
//! — they already consume the ecosystem-neutral `DependencyRecord`/
//! `DependentHit` shape.
//!
//! Deliberately **not** part of this seam: manifest-format-specific
//! inheritance/workspace resolution (Cargo's `[workspace.dependencies]` +
//! `{ workspace = true }`, npm's workspace protocol, Poetry's path/group
//! dependencies), and graph-node enrichment (renamed-package aliases,
//! git/path source detail, declaring-package context). Those stay layered on
//! top of `declarations()`'s raw per-manifest output inside each ecosystem's
//! own extraction module (`cargo_manifest.rs` for Cargo), because the shape
//! of "does this ecosystem have inheritable shared dependencies, and how" is
//! itself ecosystem-specific — baking it into the trait would make the seam
//! Cargo-shaped instead of neutral.

use std::path::{Path, PathBuf};

/// A manifest file discovered by a [`ManifestDependencyExtractor`] under a
/// project root (e.g. one `Cargo.toml` in a Cargo workspace).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestPath(pub PathBuf);

impl ManifestPath {
    /// The discovered manifest's path, as returned by `discover` (absolute
    /// when `discover` was given an absolute root, matching
    /// `discover_cargo_manifest_paths`' existing contract).
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// One ecosystem-neutral dependency declaration read from a manifest, before
/// any ecosystem-specific inheritance resolution or graph-node enrichment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyDeclaration {
    /// The dependency key as written in the manifest (may be a rename).
    pub name: String,
    /// The resolved/renamed package name, when the manifest declares one
    /// distinct from `name` (Cargo's `package = "..."`).
    pub package_name: Option<String>,
    /// The declared version requirement, if any.
    pub version_req: Option<String>,
    /// The dependency source (`registry` / `path` / `git`).
    pub source: String,
    /// The dependency table/field it was declared in
    /// (`normal` / `dev` / `build` for Cargo).
    pub kind: String,
    /// The manifest that declared it (matches
    /// [`ManifestPath::as_path`]'s contract).
    pub manifest_path: PathBuf,
}

/// A manifest-format dependency extractor: discovers manifests of one
/// ecosystem under a project root, then reads each manifest's raw declared
/// dependencies. See the module docs for what deliberately stays outside
/// this seam.
pub trait ManifestDependencyExtractor {
    /// Find every manifest of this ecosystem under `root`.
    fn discover(&self, root: &Path) -> Vec<ManifestPath>;
    /// Read one manifest's declared dependencies.
    fn declarations(&self, manifest: &ManifestPath) -> Vec<DependencyDeclaration>;
}
