# Changelog

## 0.4.1 - 2026-05-07

### Added

- Added symbol annotations, global annotation storage, branch/project-scoped annotation identity, annotation HTTP APIs, and annotation sync CLI support.
- Added project-scoped annotation service behavior, global Grapha config, configurable annotation sync server resolution, and reusable annotation workflow documentation.
- Added best-effort polyglot tree-sitter extraction for additional languages beyond Swift and Rust.
- Added temporary Grapha store migration for bootstrapping a worktree from another local store.
- Added CLI output limits, defaulting high-volume query commands to 20 shown items.
- Added extraction surface metadata and doc-comment business context in search results.

### Changed

- Made annotation records project-scoped so notes survive normal branch switches.
- Added configurable graph serve defaults through project/global config.
- Reworked the English and Chinese READMEs with a clearer product narrative, current command coverage, MCP setup, configuration guidance, and supported-language accuracy.
- Expanded CLI help with richer root workflow guidance and examples for indexing, serving, symbol search/context/impact, dataflow, concepts, repository checks, and annotation sync.
- Added help-output regression coverage for the new CLI examples.

### Fixed

- Bounded CJK fuzzy concept search by characters to avoid incorrect matching behavior.

## 0.3.0 - 2026-04-22

### Added

- Added business concept resolution commands so natural-language concepts can be searched, inspected, aliased, and bound to symbols.
- Added cached repository smell queries, plus a `--no-cache` escape hatch for fresh analysis runs.
- Added richer SwiftUI body-structure detection to improve structural understanding during extraction.

### Changed

- Improved CLI resolution for smell, asset, and localization commands.
- Strengthened concept scope recall and restored config-driven semantic overrides in the pipeline.

### Fixed

- Scoped Rust symbol IDs correctly and invalidated stale extraction cache entries to avoid cross-run contamination.
- Aligned the semantic extraction plumbing and cleaned up CI regressions on the release branch.
