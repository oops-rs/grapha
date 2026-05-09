---
name: grapha-quality
description: "Use when work in this repo needs blast-radius checks, type complexity, code smells, or module size and coupling analysis."
---

# Grapha — quality & change risk

## Tool map

| Task | MCP tool | CLI fallback |
|---|---|---|
| Blast radius / dependents of a symbol | `mcp__grapha__get_impact` | `grapha symbol impact <symbol> --depth 3` |
| Structural complexity of a type | `mcp__grapha__analyze_complexity` | `grapha symbol complexity <type>` |
| Code smells across repo / module / file / symbol | `mcp__grapha__detect_smells` | `grapha repo smells [--module ...] [--file ...] [--symbol ...]` |
| Module size & cross-module coupling | `mcp__grapha__get_module_summary` | `grapha repo modules` |

## When to run each

- **Before modifying any public API or shared type:** `get_impact`. Look at direct dependents (depth 1), indirect dependents (depth 2+), and whether any entry points are reached.
- **Before refactoring a type:** `analyze_complexity`. Reports property count, method count, dependency count, invalidation sources, init parameter count, extensions, blast radius, and an overall severity rating.
- **When prioritizing cleanup or scoping a refactor PR:** `detect_smells`. Reports god types (>15 properties), excessive deps (>10), wide invalidation surfaces (>5 sources), massive inits (>8 params), deep nesting (>5), high fan-out/fan-in (>15), many extensions (>5). Sorted by severity.
- **Before architectural decisions (split a crate, move a module):** `get_module_summary`. Compares per-module symbol count, file count, kind breakdown, edge count, cross-module coupling ratio, entry points, terminals.

## Recipes

- Scope smells to a hot area: `mcp__grapha__detect_smells({ module: "Room" })` or `{ file: "RoomPage.swift" }` or `{ symbol: "RoomPageViewModel" }`. Don't run a repo-wide smell scan when a focused one will do.
- Cluster long impact/smell lists with `cluster: true`, then page through `cluster_id: "excellent"|"strong"|"possible"|"weak"` instead of reading one giant list.
- Pair `get_impact` with `get_symbol_context` (from `grapha-search`): impact tells you *what* breaks, context tells you *how* the symbol is shaped today.

## Tips

- Severity ratings come from the tool — don't second-guess them by hand-counting. If a type is rated "high severity", trust the rating and read the metrics to understand which dimension drove it.
- A complexity report with high "invalidation source count" usually means observable-property soup — that's a refactor candidate even if the property count looks fine.
- High cross-module coupling in `get_module_summary` is a smell signal at the architectural level; pair it with a smell scan filtered to the offending modules.
