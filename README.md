# Grapha

[中文文档](docs/README.CN.md)

**Blazingly fast** code intelligence that gives AI agents compiler-grade understanding of your codebase.

Grapha builds a symbol-level dependency graph from source code — not by guessing with regex, but by reading the strongest source of structure available. For Swift, it taps directly into Xcode's pre-built index store via binary FFI for 100% type-resolved symbols, then enriches with tree-sitter for view structure, docs, localization, and asset references. For Rust, it uses tree-sitter with Cargo workspace awareness. Other common languages use best-effort tree-sitter extraction for symbols, containment, imports, and name-based calls. The result is a queryable graph with confidence-scored edges, dataflow tracing, impact analysis, code smell detection, and business-concept lookup — available as both a CLI and an MCP server for AI agent integration.

> **1,991 Swift files — 131K nodes — 784K edges — 8.7 seconds.** Zero-copy binary FFI. Lock-free parallel extraction. No serde on the hot path.

## Why Grapha

| | Grapha |
|---|---|
| **Parsing** | Compiler index store (confidence 1.0) + tree-sitter fallback |
| **Relationship types** | 10 (calls, reads, writes, publishes, subscribes, inherits, implements, contains, type_ref, uses) |
| **Dataflow tracing** | Forward (entry → terminals) + reverse (symbol → entries) |
| **Code quality** | Complexity analysis, smell detection, module coupling metrics |
| **Confidence scores** | Per-edge 0.0–1.0 |
| **Terminal classification** | Auto-detects network, persistence, cache, event, keychain, search |
| **MCP tools** | 17 |
| **Watch mode** | File watcher with debounced incremental re-index |
| **Recall** | Session disambiguation — ambiguous symbols auto-resolve after first use |

## Performance

Benchmarked on a production iOS app (1,991 Swift files, ~300K lines):

| Phase | Time |
|-------|------|
| Extraction (index store + tree-sitter enrichment) | **3.5s** |
| Merge (module-aware cross-file resolution) | 0.3s |
| Classification (entry points + terminals) | 1.7s |
| SQLite persistence (deferred indexing) | 2.0s |
| Search index (BM25 via tantivy) | 1.0s |
| **Total** | **8.7s** |

**Graph:** 131,185 nodes · 783,793 edges · 2,983 entry points · 11,149 terminal operations

**Why it's fast:** zero-copy index store FFI via pointer arithmetic (no serde), lock-free rayon extraction, single shared tree-sitter parse, marker-based enrichment skip, deferred SQLite indexing, USR-scoped edge resolution. Run `grapha index --timing` for a per-phase breakdown.

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
# Index a project (incremental by default)
grapha index .

# Check index freshness
grapha repo status

# Search symbols
grapha symbol search "ViewModel" --kind struct --context --fields full
grapha symbol search "send" --kind function --module Room --fuzzy --declarations-only
grapha symbol search "ProfileAPI" --repo FrameUI --fields file,repo,locator

# 360° context — callers, callees, reads, implements
grapha symbol context RoomPage --format tree
grapha symbol context RoomPage --format brief

# Impact analysis — what breaks if this changes?
grapha symbol impact GiftPanelViewModel --depth 2 --format tree

# Complexity analysis — structural health of a type
grapha symbol complexity RoomPage

# Dataflow: entry point → terminal operations
grapha flow trace RoomPage --format tree

# Reverse: which entry points reach this symbol?
grapha flow trace sendGift --direction reverse

# Origin tracing: which API/data source feeds this UI?
grapha flow origin UserProfileView --terminal-kind network --format tree

# Code smell detection
grapha repo smells --module Room
grapha repo smells --file Modules/Room/Sources/Room/View/RoomPage+Layout.swift
grapha repo smells --symbol RoomPageCenterContentView --no-cache

# Module metrics — symbol counts, coupling ratios
grapha repo modules

# Architecture guard — configured layer dependency rules
grapha repo arch --format brief

# Business concept lookup
grapha concept search "送礼横幅" --format tree
grapha concept bind "送礼横幅" --symbol GiftBannerPage --symbol GiftBannerViewModel

# MCP server for AI agents (with auto-refresh)
grapha serve --mcp --watch
```

## MCP Server — 17 Tools for AI Agents

```bash
grapha serve --mcp              # JSON-RPC over stdio
grapha serve --mcp --watch      # + auto-refresh on file changes
grapha index . && grapha serve --mcp --watch
```

Add to `.mcp.json`:

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

| Tool | What it does |
|------|-------------|
| `search_symbols` | BM25 search with kind/module/file/role/fuzzy filters |
| `get_index_status` | Index timestamp, repo snapshot metadata, and stale-result hints |
| `get_symbol_context` | 360° view: callers, callees, reads, implements, contains tree |
| `get_impact` | BFS blast radius at configurable depth |
| `get_file_map` | File/symbol map organized by module and directory |
| `trace` | Forward dataflow to terminals, or reverse to entry points |
| `get_file_symbols` | All declarations in a file, by source position |
| `batch_context` | Context for up to 20 symbols in one call |
| `analyze_complexity` | Structural metrics + severity rating for any type |
| `detect_smells` | Code smell scan scoped to the repo, a module, a file, or a symbol |
| `get_module_summary` | Per-module metrics with cross-module coupling ratio |
| `search_concepts` | Fuzzy business concept lookup across bindings, localization, assets, and symbols |
| `get_concept` | Stored concept aliases and bound symbols |
| `bind_concept` | Persist confirmed concept-to-symbol mappings |
| `add_concept_alias` | Add aliases for a concept |
| `remove_concept` | Remove a concept from the project concept store |
| `reload` | Hot-reload graph from disk without restarting the server |

**Recall:** The MCP server remembers symbol resolutions within a session. If `helper` is ambiguous the first time, after you disambiguate with `File.swift::helper`, future bare `helper` queries resolve automatically. Use `reload` after a manual `grapha index` run when the server is not running with `--watch`.

## Commands

### Symbols

```bash
grapha symbol search "query" [--limit N] [--kind K] [--module M] [--repo R] [--file GLOB] [--role ROLE]
grapha symbol search "query" [--fuzzy] [--exact-name] [--declarations-only] [--public-only]
grapha symbol search "query" [--context] [--fields file,id,module,repo,snippet]
grapha symbol context <symbol> [--format json|tree|brief] [--fields full]
grapha symbol impact <symbol> [--depth N] [--format json|tree|brief] [--fields file,module,repo]
grapha symbol complexity <symbol>          # property/method/dependency counts, severity
grapha symbol file <path>                  # list declarations in a file
```

### Annotations

```bash
grapha annotation serve --port 8080                    # standalone LAN annotation service
grapha annotation list                                 # local annotation records + project identity
grapha annotation sync                                 # sync with configured annotation service
grapha annotation sync --server http://HOST:8080       # explicit one-off service override
```

Annotation records are scoped by project id, not branch, so notes survive normal
branch switches. The project id comes from `[repo].name`, Git remote/common-dir
identity, or the project path fallback; set `[repo].name` for stable sync across
non-Git copies. Existing branch-specific records remain readable and are
normalized into project-scoped records on write/sync. `sync` resolves the service
address from `--server`, `GRAPHA_ANNOTATION_SERVER`, project `grapha.toml`, then
global Grapha config. `list` and `sync` use the current directory by default;
pass `--path` only when operating on another project. `annotation serve` is not
bound to a project and does not require `grapha index`; sync requests carry the
project id used to route records into the right global annotation store.

### Dataflow

```bash
grapha flow trace <symbol> [--direction forward|reverse] [--depth N] [--format json|tree|brief]
grapha flow graph <symbol> [--depth N] [--format json|tree]       # semantic effect graph
grapha flow origin <symbol> [--terminal-kind network|persistence|cache|event|keychain|search]
grapha flow entries [--module M] [--file PATH] [--limit N] [--format json|tree]
```

### Repository

```bash
grapha repo status                         # index freshness and snapshot metadata
grapha repo smells [--module M | --file PATH | --symbol QUERY] [--format json|brief] [--no-cache]
grapha repo modules                        # per-module metrics
grapha repo map [--module M]               # file/symbol overview
grapha repo changes [unstaged|staged|all|REF]
grapha repo arch [--format json|brief]     # configured architecture rule violations
grapha repo infer [--format json|brief]    # opt-in inferred module/ownership/doc metadata
grapha repo doctor [--format json|brief]   # graph and inferred metadata health checks
grapha repo history add --kind test --title "cargo test" [--file PATH] [--module M] [--symbol QUERY]
grapha repo history list [--kind test] [--file PATH] [--module M] [--symbol QUERY] [--limit N]
```

### Indexing & Serving

```bash
grapha index <path> [--format sqlite|json] [--store-dir DIR] [--full-rebuild] [--timing]
grapha migrate [-p PATH] [--from OTHER_WORKTREE_OR_STORE] [--force]
grapha analyze <path> [--compact] [--filter fn,struct]
grapha serve [-p PATH] [--mcp] [--watch[=true|false]] [--host HOST] [--port N]
```

### Localization & Assets

```bash
grapha l10n symbol <symbol> [--format json|tree]
grapha l10n usages <key> [--table T] [--format json|tree]
grapha asset list [--unused]               # image assets from xcassets catalogs
grapha asset usages <name> [--format json|tree]
```

### Concepts

```bash
grapha concept search "送礼横幅" [--limit N] [--format json|tree]   # fuzzy by default, limit defaults to 20
grapha concept show "送礼横幅" [--format json|tree]
grapha concept bind "送礼横幅" --symbol GiftBannerPage --symbol GiftBannerViewModel
grapha concept alias "送礼横幅" --add "礼物 banner" --add "gift banner"
grapha concept remove "送礼横幅"
grapha concept prune                       # drop bindings to missing symbols
```

## Configuration

Optional `grapha.toml` at project root:

```toml
[repo]
name = "MobileApp"                         # defaults to the project directory name

[annotations]
server = "http://192.168.1.10:8080"        # default for `grapha annotation sync`

[serve]
host = "0.0.0.0"                           # default bind host for `grapha serve`
port = 18081                               # default HTTP port for this project
watch = true                               # auto-refresh graph while serving

[swift]
index_store = true                         # false → tree-sitter only

[output]
default_fields = ["file", "module", "repo"]

[inferred]
enabled = false                            # true enables `grapha repo infer`

[[external]]
name = "FrameUI"
path = "/path/to/local/frameui"            # include in cross-repo analysis

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

Global developer defaults can live in `$GRAPHA_CONFIG`,
`$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or
`~/.grapha/config.toml`. Project `grapha.toml` overrides global config for
repo-specific values such as `[annotations].server` and `[serve].port`.

## Architecture

```
grapha-core/     Shared types (Node, Edge, Graph, ExtractionResult)
grapha-rust/     Rust plugin and tree-sitter extractor
grapha-swift/    Swift: index store → SwiftSyntax → tree-sitter waterfall
grapha/          CLI, query engines, MCP server, web UI
nodus/           Agent tooling package (skills, rules, commands)
```

### Extraction Waterfall (Swift)

```
Xcode Index Store (binary FFI)      → compiler-resolved USRs, confidence 1.0
  ↓ fallback
SwiftSyntax (JSON FFI)              → accurate parse, no type resolution, confidence 0.9
  ↓ fallback
tree-sitter-swift (bundled)         → fast, limited accuracy, confidence 0.6–0.8
```

After index store extraction, tree-sitter enriches doc comments, SwiftUI view hierarchy, and localization metadata in a single shared parse.

### Graph Model

**16 node kinds:** function, class, struct, enum, trait, impl, module, field, variant, property, constant, type_alias, protocol, extension, view, branch

**10 edge kinds:** calls, implements, inherits, contains, type_ref, uses, reads, writes, publishes, subscribes

**Dataflow annotations:** direction (read/write/pure), operation (fetch/save/publish), condition, async_boundary, provenance (source file + span)

**Node roles:** entry_point (SwiftUI View, @Observable, fn main, #[test]) · terminal (network, persistence, cache, event, keychain, search)

### Nodus Package

```bash
nodus add wenext/grapha --adapter claude
```

Installs skills, rules, and slash commands (`/index`, `/search`, `/impact`, `/complexity`, `/smells`) for grapha-aware AI workflows.

## Supported Languages

| Language | Extraction | Type Resolution |
|----------|-----------|----------------|
| **Swift** | Index store + tree-sitter | Compiler-grade (USR) |
| **Rust** | tree-sitter | Name-based |
| TypeScript / TSX / JavaScript | best-effort tree-sitter | Name-based |
| Python / Go / Java / C / C++ / C# | best-effort tree-sitter | Name-based |
| PHP / Ruby / Kotlin / Dart / Pascal | best-effort tree-sitter | Name-based |

Swift and Rust remain first-class extractors. The additional languages share one generic tree-sitter path so they can provide useful graph coverage without pretending to have compiler-grade semantics.

## Development

```bash
cargo build                    # Build all workspace crates
cargo test                     # Run the workspace test suite
cargo clippy && cargo fmt      # Lint + format
```

## License

MIT
