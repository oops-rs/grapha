# Tree-Sitter Path Quality

Date: 2026-07-24

## Purpose

Grapha's strongest extraction lives in two hand-built paths: Swift (index store /
SwiftSyntax / a rich tree-sitter fallback) and Rust (a ~3,000-line tree-sitter
extractor with Cargo-aware context). Every other language — TypeScript,
JavaScript, Python, Go, Java, Kotlin, C, C++, C#, PHP, Ruby, Dart, Pascal — rides
the generic config-table walker in `grapha-core/src/tree_sitter.rs`. Nous treats
Grapha as its general-purpose code substrate, and its capability model already
encodes the gap: tree-sitter-only languages get `concept = partial` and
`impact`/`trace = unsupported`. This plan closes the quality gap from the
extraction side so those tiers can be raised, and so Swift-on-Linux (which always
runs the tree-sitter fallback) keeps improving with the same work.

The audit behind this plan read the generic walker, both strong extractors, and
the live resolver end to end, and verified each defect empirically with
`grapha analyze` on micro-fixtures.

## First Principles

Facts already true:

- The live cross-file resolver is `grapha-core/src/merge.rs` and it is
  language-agnostic and good: import-guided binding, `Type::method` member
  resolution, tiered confidence multipliers (same-module unique x0.9,
  hint-narrowed x0.85, imported-unique x0.8, import-bound x0.75, cross-module
  imported x0.7, ambiguous x0.4/x0.3 or drop). Extraction quality, not
  resolution, is the bottleneck.
- The strong extractors use the same architecture as the generic walker: pure
  tree-sitter, same-file name-based targets, cross-file work deferred to merge.
  Nothing about the generic path is architecturally capped.
- Edge confidence is a ranking signal only (`cluster.rs` edge scores); traversal
  and search never gate on it. Generic edges carry base 0.55–0.6 versus Rust
  0.7–0.9 and Swift 0.8–0.9, so polyglot evidence permanently ranks last.
- `grapha-engine/src/merge.rs`, `classify/pass.rs`, `classify/swift.rs`, and the
  `Classifier` trait machinery are dead code with zero non-test callers. The only
  live classify entry points are `classify::rust::terminal_effect_for_target` and
  `classify::android::terminal_effect_for_target`.

Verified defects in the generic walker (`grapha-core/src/tree_sitter.rs`):

- Calls inside variable initializers are dropped: `const name = formatName("x")`
  produces no call edge. `extract_variable_declarator` returns without walking
  the initializer expression unless it is an arrow/function literal.
- Calls inside class arrow-function fields are dropped: fields are extracted as
  leaves (`walk` returns after `extract_leaf`) and their bodies are never walked.
- Top-level calls are dropped: `extract_call` requires an enclosing
  Function/Property scope, so script-style Python/JS and module-init code emit
  nothing.
- Dart emits zero call edges (`call_types: EMPTY` in
  `grapha-engine/src/polyglot_plugin.rs`).
- Go structs and interfaces both surface as `type_alias` (only `type_spec` is
  configured), and methods have no association with their receiver type.
- Visibility is a substring scan over the whole declaration text: `export class
  Widget` reads as Private (the wrapping `export_statement` is not the scanned
  node), while any body containing the word "public" reads as Public. The
  async/static/exported metadata flags use the same fragile scan.
- Doc comments only capture `//`-style line runs before a declaration. Javadoc,
  JSDoc, and KDoc `/** ... */` blocks are never captured; Python docstrings
  (inside the body) are structurally unreachable for the current scan.
- `Import.symbols` is always empty, so merge's import-guided tiers cannot use
  named imports (`import { formatName } from "./util"` keeps only the module
  path). The Swift extractor shares this gap.
- Local variables become graph nodes (`run::name`, `run::store`), polluting
  symbol search.
- Inheritance combines AST field lookups with a raw-text scan for
  `extends`/`implements`/`:` over the whole container text, and
  implements-vs-inherits is guessed from name shape (`I*`, `*able`,
  `*Protocol`).
- TS and C# enum members are not extracted; Kotlin interface/enum detection is a
  raw-text prefix heuristic; C++ namespaces are not containers, so their contents
  flatten to file scope.
- Semantic roles (entry points, terminal effects) exist only for Rust, Swift,
  and Android-flavored Kotlin/Java. Every other language gets zero role
  classification, which is exactly what `impact`/`trace` need.
- Module discovery understands Cargo, SPM, and Gradle. There is no npm workspace,
  `go.mod`, or CMake awareness, so those ecosystems lose merge's module-scoped
  resolution tiers.

Constraints:

- Pure tree-sitter plus deterministic heuristics. No rust-analyzer, no LSP, no
  type inference beyond what syntax makes evident, no per-language compiler
  services. The native Swift tiers stay untouched.
- Extraction stays per-file and cache-friendly; cross-file knowledge stays in
  merge. Existing `ExtractionResult` shapes and store schemas keep working.
- Precision before confidence: base confidences only rise where a measured
  false-positive rate justifies it.

Invariants:

- Every emitted edge keeps provenance to a source span.
- A fix for one language lands in the shared walker whenever the defect is
  shared; per-language code is reserved for genuinely per-language idioms.
- Every phase lands with fixture coverage that would have caught the defect it
  fixes.

## Non-Goals

- Replacing the config-table walker wholesale with per-language extractors.
  The Swift and Rust extractors earn their line count; the middle tier should
  stay declarative with small hooks.
- Embedding-based or LLM-based extraction. This plan is about the deterministic
  floor.
- Query-surface changes. Consumers see better graphs through existing commands.

## Roadmap

### P0 — Measurement harness

The current `polyglot_test.rs` asserts one function and one call per language;
`quality_benchmark.rs` is Rust-only and measures latency/size. Neither would
catch any defect listed above. Merge silently drops unresolvable edges, so
recall loss is invisible today.

1. **Golden fixtures per language.** A small idiomatic file set per supported
   language exercising: initializer calls, class fields with function values,
   top-level calls, block doc comments, docstrings, visibility modifiers,
   enums with members, inheritance/conformance, named imports, receivers
   (Go/Kotlin), namespaces (C++/C#). Assertions on nodes, edges, docs,
   visibility — not just "a function exists".
2. **Extraction-quality metrics.** A test-harness report (and later a
   `grapha doctor`-style command) computing per language: % of
   functions with at least one outgoing call edge, merge-time
   unresolved-drop rate, % of documented declarations whose doc_comment was
   captured, node-kind histogram. Baselines checked in so regressions and
   improvements are both visible.
3. **Real-corpus baseline.** Run the metrics once against a real TS repo, a real
   Kotlin repo, and a Rust workspace (Rust as the reference ceiling), and record
   the numbers in this doc.

Acceptance: metrics exist, run in CI on fixtures, and the baseline numbers for
the defect areas above are recorded.

### P1 — Shared-walker correctness

All languages benefit at once; no config changes required.

1. **Walk initializer expressions.** After extracting a variable/field leaf,
   continue walking its value expression so calls inside initializers and class
   arrow-function fields are captured. Attribute to the nearest enclosing
   callable scope.
2. **File-scope call attribution.** When no callable scope encloses a call,
   attribute it to the file node instead of dropping it.
3. **AST-based doc comments.** Collect preceding comment siblings from the tree
   (line and block), skipping attributes/annotations/decorators, mirroring the
   Rust extractor. Special-case Python docstrings (first string expression of a
   body).
4. **Modifier-based visibility and flags.** Read visibility/async/static from
   modifier child nodes and wrapping nodes (`export_statement` ancestors) per
   grammar, replacing the substring scans.
5. **Populate `Import.symbols`.** Parse named imports (`import { a, b }`,
   `from m import a`, `use m::{a, b}`-style lists) so merge's import-guided
   binding can fire on symbol-level evidence. Applies to the Swift extractor's
   import pass too.
6. **Stop emitting local variables** as graph nodes (keep walking their
   initializers). Module/file-level variables and constants stay.
7. **AST-only inheritance.** Drop the raw-text `extends/implements/:` scan;
   per-language field names in config. Let merge decide implements-vs-inherits
   from what the target resolves to, falling back to Inherits, and retire the
   name-shape guess.
8. **Emit `operation` chain hints** for qualified calls/reads (the prefix chain
   the Swift extractor already stashes) so merge's hint-narrowed tier (x0.85)
   works for polyglot languages.
9. **Confidence re-baseline.** With the precision fixes in, raise generic call
   edges toward the 0.8 the strong extractors use, guarded by the P0
   false-positive measurements.

Acceptance: P0 metrics move on fixtures — near-zero dropped-call rate for
initializer/top-level cases, doc capture for block comments and docstrings,
correct visibility for exported/public symbols — with no unresolved-drop-rate
regression.

### P2 — Per-language idioms

Add a small per-language hook trait next to `TreeSitterLanguageConfig` (name
resolution, container classification, receiver association) so idiom fixes have
a home that is not the shared walker:

1. **Go:** `struct_type`/`interface_type` under `type_spec` classify as
   Struct/Trait; method receivers associate the method with its type (Contains
   or a dedicated association edge); `go.mod` module discovery.
2. **Kotlin:** grammar-node interface/enum detection (replace text-prefix
   heuristics); extension-function receiver recorded in metadata; `kts` covered
   by fixtures.
3. **TypeScript/JavaScript:** enum members; decorator capture (metadata);
   namespace/module containers; re-export handling in imports.
4. **C#:** enum members; records; namespace containers.
5. **C++:** namespace containers; associate out-of-class `Class::method`
   definitions with their class.
6. **Dart:** call extraction (`call_types` plus selector chains), variable
   declarations.
7. **Python:** decorator capture; dataclass/attrs fields as Fields.

Acceptance: per-language fixture suites pass; node-kind histograms for Go and
Kotlin match their language reality (structs are Structs, interfaces are
Traits).

### P3 — Semantic roles for the big ecosystems

`extract_semantics` for polyglot currently annotates only Android sources.
Entry points and terminal effects are what `impact`/`trace`/`concept` consume,
so this phase is what unlocks capability-tier raises in consumers.

1. **Entry points:** web routes already detected as Route nodes get EntryPoint
   roles wired to their handler functions (Express/Nest, Flask/FastAPI/Django,
   Go net/http and the router regexes already present, Rails, Laravel); `main`
   functions; test functions; React components already detected.
2. **Terminal effects:** per-ecosystem tables mirroring `classify/android.rs`
   and `classify/rust.rs` for filesystem, database, and network libraries in
   the JS/TS, Python, and Go ecosystems first.
3. **Consumer follow-up (nous-side, tracked there):** graduate
   `graph_depth_for_language` from the binary deep/tree-sitter-only split to a
   per-language table so upgraded languages regain `concept`/`impact`/`trace`.

Acceptance: entry-point and terminal coverage measurable on fixtures for TS,
Python, and Go; a real-corpus `impact` query on a TS repo returns a
non-degenerate result.

### P4 — Infrastructure polish

1. **Dead-code removal:** delete `grapha-engine/src/merge.rs`,
   `classify/pass.rs`, `classify/swift.rs`, and the unused `Classifier`
   machinery so future resolution work cannot land in the wrong file.
2. **Unresolved-edge accounting:** merge records counts of dropped edges per
   reason (no candidate, ambiguous >3 files) into index status/manifest so the
   doctor metrics can read them without re-deriving.
3. **Module discovery:** npm/pnpm workspaces and CMake, following the `go.mod`
   work from P2.
4. **ID stability:** replace the span-suffix collision fallback with a stable
   overload index (`name#2` within scope) so edits above a symbol do not rename
   colliding siblings.

Acceptance: workspace tests green after dead-code removal; unresolved counts
visible in index status; overload IDs stable across unrelated edits in
fixtures.

## Sequencing And Risk

P0 before everything: every later phase cites its numbers. P1 is the highest
value-per-line and is confined to `grapha-core/src/tree_sitter.rs` plus the
import parser; its main risk is precision loss from newly-captured calls, which
the P0 false-positive metrics guard. P2 items are independent of each other and
can land per language. P3 depends on P1/P2 only for edge quality, not for
mechanism, and can start with TS. P4 is independent except item 2, which wants
P0's metric definitions.

The Swift native tiers and the Rust extractor are untouched throughout; the
Swift tree-sitter fallback picks up P1 items 5 and 8 and otherwise stays as the
existing quality bar for what a tree-sitter-only extractor can be.
