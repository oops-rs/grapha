# Rust usage evidence

Source: the Mentra test audit found zero `SessionResumeOptions` usages in a fresh
index despite six construction sites. The user authorized updating Grapha.

## Facts and invariants

- Rust extraction uses tree-sitter and cannot prove complete compiler resolution.
- The reproducer emits a return type reference but omits struct literals and
  parameter types; destructuring creates a variable named after the type.
- Graph edges already support construction, type references, and source spans.
- A usage result describes resolved indexed relationships, not proof of non-use.
- MCP can retain an older loaded graph while disk metadata is replaced. Freshness
  must refer to the loaded graph's snapshot, not whichever snapshot is newest.

## Scope

Implement usage correctness and resolution/freshness reporting first. Test
assertion analysis, duplicate-test suggestions, and persisted mutation evidence
are deferred. Keep existing response data accessible and preserve conservative
resolution: explicit imports/paths must not bind unrelated same-name symbols.

## Slices and ownership

1. Rust extraction and fixture regressions: specialist owns
   `grapha-engine/src/extract/rust*` and the focused integration test.
2. Shared merge resolution: coordinator owns `grapha-core/src/merge.rs` and
   dedicated regression fixtures. This depends on emitted edge shapes from 1.
3. Query evidence and snapshot lifecycle: specialist owns usage result types,
   index status snapshot access, CLI/MCP response state and tests. No extractor
   or core merge overlap.
4. Integrated verification and independent review, then update the selected
   Grapha checkout with the verified result. No remote publication requested.

The read/implementation lanes above have disjoint file ownership. All Cargo
execution, commits, integrations, and artifact cleanup are coordinator-owned.
Worktree: `/private/tmp/grapha-rust-usage-evidence`, branch
`fix/rust-usage-evidence`, base `23a739a`. Selected checkout:
`/Users/wendell/developer/oops-rs/grapha`.

## Verification and ablation

- Focused Rust fixtures cover literals, parameters, destructuring, aliases,
  same-name decoys, and source locations.
- Query evidence tests cover zero resolved matches, stale source inputs, disk
  replacement while MCP retains an old graph, and coherent reload/watch swaps.
- Verify the fresh binary against real Mentra and inspect all six construction
  sites without modifying its source or existing index.
- Temporarily remove key fixes and confirm new regression tests fail; restore.
- Run workspace tests, feature variants, doctests, strict Clippy, formatting,
  and warnings-denied docs with task-owned build artifacts. Rust-only checks
  disable automatic Swift bridge compilation; report platform-specific limits.
