---
name: grapha-workflow
description: Use grapha for symbol search, context lookup, complexity analysis, and impact assessment before reading full files or modifying code
---

# Grapha Workflow

Use grapha's code intelligence to navigate, understand, and assess codebases before making changes.

## When to use

- Before exploring an unfamiliar part of the codebase
- Before modifying public APIs or shared types
- When assessing code quality or refactoring candidates
- When orienting in a large project

## Core workflow

1. **Search first:** `grapha symbol search "<query>" --context` to find relevant symbols with full indexed snippets
2. **Understand relationships:** `grapha symbol context <symbol>` to see callers, callees, and dependencies
3. **Check impact before changes:** `grapha symbol impact <symbol>` to understand blast radius
4. **Assess complexity:** `grapha symbol complexity <type>` to check structural health of a type
5. **Orient in large projects:** `grapha repo modules` for per-module metrics, `grapha repo map` for file layout
6. **Preserve agent knowledge:** `grapha symbol annotate <symbol> "<note>" --by <agent>` to store durable symbol notes

## Quality assessment

- `grapha repo smells` — scan the full graph for code smells (god types, deep nesting, wide invalidation, excessive fan-out)
- `grapha repo smells --module Room` — scope to a single module
- `grapha symbol complexity <type>` — detailed metrics for a specific type (properties, dependencies, init params, invalidation sources)

## Annotation service and sync

- Record an annotation when you discover reusable symbol knowledge that would be expensive to re-derive later, such as ownership, business role, invariants, cross-module coupling, or migration context. This can reduce future token usage by letting agents retrieve a compact note instead of rereading surrounding files.
- Keep annotations concise and factual. Do not annotate transient guesses, obvious names, or task-local scratch notes.
- `grapha annotation serve -p . --port 8080` — deploy the local HTTP annotation service; the Grapha HTTP server binds for LAN access
- `grapha annotation list` — inspect local annotation records and their project/branch identity
- `grapha annotation sync` — pull, merge, and push annotations against the configured Grapha annotation service
- `grapha annotation sync --server http://HOST:8080` — override the configured service for one sync
- `grapha annotation sync` resolves the service address from `--server`, then `GRAPHA_ANNOTATION_SERVER`, then project `grapha.toml`, then global Grapha config.
- Global config can live at `$GRAPHA_CONFIG`, `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml`.
- `list` and `sync` use the current directory by default; pass `--path` only when operating on another project.
- Annotation records are scoped by project id and branch, while retaining fallback behavior for legacy/shared records. Set `[repo].name` in `grapha.toml` when syncing non-Git copies that should share one project identity.

## Dataflow tracing

- `grapha flow trace <symbol>` — follow data forward from a symbol to terminals (network, persistence, etc.)
- `grapha flow trace <symbol> --direction reverse` — find which entry points reach a symbol
- `grapha flow entries` — list auto-detected entry points

## Tips

- Use `--kind function` to narrow search to functions only
- Use `--module ModuleName` to search within a specific module
- Use `--file RoomPage.swift` to restrict results to a file or `--file "Sources/*/RoomPage.swift"` for a glob
- Use `--role entry_point`, `--role terminal`, or `--role internal` when symbol names are common
- Use `--fuzzy` if unsure of exact spelling
- Use `file.swift::symbol` to disambiguate when multiple symbols share a name
- After significant code changes, run `grapha index .` to keep the graph fresh and refresh stored snippets
- After resolving a non-obvious symbol's role or invariant, consider `grapha symbol annotate <symbol> "<compact note>" --by <agent>` so future agents can spend fewer tokens reloading context
- Before relying on shared annotation knowledge from another machine, run `grapha annotation sync` after configuring `[annotations].server` in project or global config, or `GRAPHA_ANNOTATION_SERVER`.
