# Grapha

[中文文档](docs/README.CN.md)

Grapha is a fast code intelligence CLI and MCP server for building a normalized, queryable graph of a codebase. It is designed for the questions developers and AI agents ask all day:

- Where is this symbol declared?
- What calls it, reads it, writes it, or implements it?
- What breaks if I change it?
- Which entry points can reach this API, database write, cache access, or event publish?
- Which files, modules, and concepts should I inspect before editing?

Grapha does not treat source as plain text. For Swift, it reads Xcode's pre-built index store through binary FFI when available, then falls back through SwiftSyntax and tree-sitter. For Rust, it uses a dedicated tree-sitter extractor with Cargo workspace awareness. Other supported languages use best-effort tree-sitter extraction for symbols, containment, imports, and name-based relationships.

> Benchmark: 1,991 Swift files, about 300K lines, 131K nodes, and 784K edges indexed in 8.7 seconds on a production iOS app.

## Why Grapha

| Need | Grapha answer |
|------|---------------|
| High-confidence Swift relationships | Xcode index store USRs when available, with confidence-scored graph edges |
| Fast fallback parsing | SwiftSyntax and bundled tree-sitter paths for useful results without a successful build |
| Agent-ready context | CLI output plus MCP tools for search, context, impact, dataflow, smells, and concepts |
| Change planning | Impact analysis, reverse traces to entry points, repo changes, and architecture checks |
| Product vocabulary | Concept lookup across bindings, aliases, localization strings, assets, and symbols |
| Local-first workflow | Persistent `.grapha/` store, incremental indexing, watch mode, and local annotation sync |

## Install

```bash
brew tap oops-rs/tap
brew install grapha
```

```bash
cargo install grapha
```

## Quick Start

```bash
# Build or refresh the project graph
grapha index .

# Check whether the stored graph is fresh
grapha repo status

# Search symbols
grapha symbol search "ViewModel" --kind struct --context --fields full
grapha symbol search "send" --kind function --module Room --fuzzy --declarations-only
grapha symbol search "ProfileAPI" --repo FrameUI --fields file,repo,locator

# Inspect a symbol neighborhood
grapha symbol context RoomPage --format tree
grapha symbol context File.swift::helper --fields full

# Estimate blast radius before changing code
grapha symbol impact GiftPanelViewModel --depth 2 --format tree

# Trace forward to terminal operations or backward to entry points
grapha flow trace RoomPage --format tree
grapha flow trace sendGift --direction reverse
grapha flow origin UserProfileView --terminal-kind network --format tree

# Scan repository health
grapha repo smells --module Room --format brief
grapha repo modules
grapha repo arch --format brief

# Find code by product language
grapha concept search "送礼横幅" --format tree
grapha concept bind "送礼横幅" --symbol GiftBannerPage --symbol GiftBannerViewModel

# Serve Grapha to AI agents through MCP
grapha serve --mcp --watch
```

## CLI Guide

### Indexing and Serving

```bash
grapha index <path> [--format sqlite|json] [--store-dir DIR] [--full-rebuild] [--timing]
grapha migrate [-p PATH] [--from OTHER_WORKTREE_OR_STORE] [--force]
grapha analyze <path> [--output FILE] [--compact] [--filter fn,struct]
grapha serve [-p PATH] [--mcp] [--watch[=true|false]] [--host HOST] [--port N]
```

- `index` is the normal entry point. It stores graph data, search data, localization snapshots, asset snapshots, and freshness metadata under `.grapha/` by default.
- `migrate` bootstraps a worktree from another local Grapha store so a fresh branch can answer queries before a full rebuild.
- `analyze` emits an immediate graph for one-off inspection.
- `serve` runs either the HTTP graph explorer or an MCP server over stdio.

### Symbol Intelligence

```bash
grapha symbol search "query" [--limit N] [--kind K] [--module M] [--repo R] [--file GLOB] [--role ROLE]
grapha symbol search "query" [--fuzzy] [--exact-name] [--declarations-only] [--public-only]
grapha symbol search "query" [--context] [--fields file,id,locator,module,repo,snippet]
grapha symbol context <symbol> [--format json|tree|brief] [--fields full] [--limit N]
grapha symbol impact <symbol> [--depth N] [--format json|tree|brief] [--fields file,module,repo] [--limit N]
grapha symbol complexity <symbol>
grapha symbol file <path>
grapha symbol annotate <symbol> "note" [--by agent]
grapha symbol annotation <symbol>
```

Use exact IDs, locators, or disambiguating forms such as `File.swift::helper` when multiple symbols share a name. Tree and brief formats are meant for humans; JSON is stable for scripts and agents.

### Dataflow

```bash
grapha flow trace <symbol> [--direction forward|reverse] [--depth N] [--format json|tree|brief]
grapha flow graph <symbol> [--depth N] [--format json|tree]
grapha flow origin <symbol> [--terminal-kind network|persistence|cache|event|keychain|search]
grapha flow entries [--module M] [--file PATH] [--limit N] [--format json|tree]
```

Forward traces start from an entry point or symbol and look for terminal operations. Reverse traces start from a symbol or terminal and find entry points that can reach it. Origin tracing is tuned for UI-to-data-source questions such as "which API feeds this screen?"

### Repository Health

```bash
grapha repo status
grapha repo changes [unstaged|staged|all|REF] [--limit N]
grapha repo map [--module M]
grapha repo modules
grapha repo smells [--module M | --file PATH | --symbol QUERY] [--format json|brief] [--no-cache]
grapha repo arch [--format json|brief]
grapha repo infer [--format json|brief]
grapha repo doctor [--format json|brief]
grapha repo history add --kind test --title "cargo test" [--file PATH] [--module M] [--symbol QUERY]
grapha repo history list [--kind test] [--file PATH] [--module M] [--symbol QUERY] [--limit N]
```

These commands help orient in large repositories: freshness, file maps, module coupling, architecture rule violations, structural smells, inferred metadata health, and typed project history.

### Concepts

```bash
grapha concept search "gift banner" [--limit N] [--format json|tree]
grapha concept show "gift banner" [--format json|tree]
grapha concept bind "gift banner" --symbol GiftBannerPage --symbol GiftBannerViewModel
grapha concept alias "gift banner" --add "送礼横幅" --add "gift banner page"
grapha concept remove "gift banner"
grapha concept prune
```

Concept search combines confirmed bindings, aliases, localization text, asset names, and symbol search. Bindings are local project data, so agents can reuse product vocabulary after a human confirms it once.

### Localization and Assets

```bash
grapha l10n symbol <symbol> [--format json|tree]
grapha l10n usages <key> [--table TABLE] [--format json|tree]
grapha asset list [--unused]
grapha asset usages <name> [--format json|tree]
```

Localization queries connect SwiftUI symbol subtrees to strings and usage sites. Asset queries index `.xcassets` catalogs and source references.

### Annotations

```bash
grapha annotation serve --port 8080
grapha annotation list [-p PATH]
grapha annotation sync [-p PATH]
grapha annotation sync --server http://HOST:8080
```

Annotations are local-first notes scoped by project identity, not by branch. Project identity comes from `[repo].name`, Git metadata, or the project path fallback. Sync resolves the service address from `--server`, `GRAPHA_ANNOTATION_SERVER`, project `grapha.toml`, then global Grapha config.

## MCP Server

```bash
grapha index .
grapha serve --mcp --watch -p .
```

Add Grapha to an MCP client:

```json
{
  "mcpServers": {
    "grapha": {
      "command": "grapha",
      "args": ["serve", "--mcp", "--watch", "-p", "."]
    }
  }
}
```

Available MCP tools:

| Tool | What it does |
|------|--------------|
| `search_symbols` | BM25 symbol search with kind/module/file/role/fuzzy filters |
| `get_index_status` | Index timestamp, repository snapshot metadata, and stale-result hints |
| `get_symbol_context` | 360-degree symbol context: callers, callees, reads, implementors, containment |
| `get_impact` | Blast radius analysis at configurable depth |
| `get_file_map` | File and symbol map organized by module and directory |
| `trace` | Forward dataflow to terminals or reverse dataflow to entry points |
| `get_file_symbols` | All declarations in a file, ordered by source position |
| `batch_context` | Context for up to 20 symbols in one call |
| `analyze_complexity` | Structural metrics and severity rating for a type |
| `detect_smells` | Code smell scan scoped to repo, module, file, or symbol |
| `get_module_summary` | Per-module metrics and cross-module coupling ratio |
| `search_concepts` | Business concept lookup across bindings, localization, assets, and symbols |
| `get_concept` | Stored concept aliases and bound symbols |
| `bind_concept` | Persist confirmed concept-to-symbol mappings |
| `add_concept_alias` | Add aliases for a concept |
| `remove_concept` | Remove a concept from the project concept store |
| `reload` | Reload graph data from disk without restarting the server |

The MCP server remembers symbol resolutions within a session. If `helper` is ambiguous, disambiguating once with `File.swift::helper` lets later bare `helper` queries resolve to the same symbol. Use `reload` after a manual `grapha index .` when the server is not running with `--watch`.

## Configuration

Project configuration lives in an optional `grapha.toml` at the project root:

```toml
[repo]
name = "MobileApp"

[annotations]
server = "http://192.168.1.10:8080"

[serve]
host = "0.0.0.0"
port = 18081
watch = true

[swift]
index_store = true

[output]
default_fields = ["file", "module", "repo"]

[inferred]
enabled = false

[[external]]
name = "FrameUI"
path = "/path/to/local/frameui"

[[architecture.layers]]
name = "ui"
patterns = ["AppUI*", "Features/*/View*"]

[[architecture.layers]]
name = "infra"
patterns = ["Networking*", "Persistence*"]

[[architecture.deny]]
from = "infra"
to = "ui"
reason = "Infrastructure must not depend on UI."

[[classifiers]]
pattern = "FirebaseFirestore.*setData"
terminal = "persistence"
direction = "write"
operation = "set"
```

Global developer defaults can live in `$GRAPHA_CONFIG`, `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml`. Project config overrides global config for repository-specific values such as `[annotations].server` and `[serve].port`.

## Architecture

```text
grapha-core/     Shared graph, extraction, semantic, selector, and plugin types
grapha-rust/     Rust plugin and tree-sitter extractor package
grapha-swift/    Swift extraction: index store -> SwiftSyntax -> tree-sitter
grapha/          CLI binary, query engines, persistence, MCP server, and web UI
nodus/           Agent tooling package with skills, rules, and commands
```

### Swift Extraction Waterfall

```text
Xcode Index Store (binary FFI) -> compiler-resolved USRs, confidence 1.0
  fallback
SwiftSyntax (JSON FFI)         -> accurate parse, no type resolution, confidence 0.9
  fallback
tree-sitter-swift              -> fast parser fallback, confidence 0.6-0.8
```

After index store extraction, tree-sitter enriches doc comments, SwiftUI view hierarchy, localization metadata, and asset references in a shared parse.

### Graph Model

- Node kinds include files, functions, methods, types, modules, imports/exports, routes, components, Swift protocols/extensions, SwiftUI view nodes, and branch nodes.
- Edge kinds include calls, uses, imports, exports, implements, contains, type references, reads, writes, publishes, subscribes, inherits, extends, instantiates, overrides, decorates, returns, and references.
- Dataflow annotations track direction, operation, condition, async boundary, and source provenance.
- Node roles distinguish entry points, terminal operations, and internal symbols.
- Terminal kinds are `network`, `persistence`, `cache`, `event`, `keychain`, and `search`.

## Performance

Benchmarked on a production iOS app with 1,991 Swift files and about 300K lines:

| Phase | Time |
|-------|------|
| Extraction, including index store and tree-sitter enrichment | 3.5s |
| Merge and module-aware cross-file resolution | 0.3s |
| Entry point and terminal classification | 1.7s |
| SQLite persistence with deferred indexing | 2.0s |
| BM25 search index via Tantivy | 1.0s |
| Total | 8.7s |

The resulting graph contained 131,185 nodes, 783,793 edges, 2,983 entry points, and 11,149 terminal operations. Use `grapha index . --timing` on your own project for a phase breakdown.

## Supported Languages

| Language | Extraction | Type resolution |
|----------|------------|-----------------|
| Swift | Xcode index store, SwiftSyntax, tree-sitter | Compiler-grade USRs when index store is available |
| Rust | Dedicated tree-sitter extractor | Name-based |
| TypeScript / TSX / JavaScript | Generic tree-sitter extractor | Name-based |
| Python / Go / Java / C / C++ / C# | Generic tree-sitter extractor | Name-based |
| PHP / Ruby / Kotlin / Dart / Pascal | Generic tree-sitter extractor | Name-based |

Swift and Rust are first-class extraction paths. Other languages provide useful structural coverage without claiming compiler-grade semantics.

## Development

```bash
cargo build                    # Build all workspace crates
cargo test                     # Run the workspace test suite
cargo clippy                   # Lint
cargo fmt -- --check           # Check formatting
```

## License

MIT
