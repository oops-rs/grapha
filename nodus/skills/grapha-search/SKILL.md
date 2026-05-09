---
name: grapha-search
description: Find symbols, read 360° context, list file symbols, and orient in unfamiliar modules using Grapha. Load when the task is "where is X defined", "who calls / what does X call", "what symbols are in this file", or "what does this module look like" — before reaching for Grep/Glob/Read.
---

# Grapha — search & orient

Discover symbols and read their relationships through Grapha instead of grepping or reading whole files. **MCP-first** (`mcp__grapha__*`); fall back to the `grapha` CLI if MCP is not mounted.

## Tool map

| Task | MCP tool | CLI fallback |
|---|---|---|
| Find symbols by name / kind / file / role | `mcp__grapha__search_symbols` | `grapha symbol search "<query>" --context` |
| 360° context: callers, callees, impls, annotations | `mcp__grapha__get_symbol_context` | `grapha symbol context <symbol>` |
| Many contexts in one shot | `mcp__grapha__batch_context` | (loop CLI calls) |
| List symbols inside a file | `mcp__grapha__get_file_symbols` | `grapha symbol search --file <path>` |
| File / module layout | `mcp__grapha__get_file_map` | `grapha repo map` |

## Search recipe

1. Start narrow:
   - MCP: `mcp__grapha__search_symbols({ query: "<q>", fields: "score,id,locator,doc_comment,annotation,signature" })`
   - CLI: `grapha symbol search "<q>" --context`
2. Retry with `fuzzy: true` (`--fuzzy`) only if there are no results.
3. Narrow noisy matches with filters: `kind` (`function`, `struct`, ...), `module`, `file` (path or glob like `Sources/*/RoomPage.swift`), `role` (`entry_point`, `terminal`, `internal`).
4. Use `exact_name: true`, `declarations_only: true`, or `public_only: true` to cut synthetic and accessor noise.
5. Disambiguate with `file.swift::symbol` syntax when several symbols share a name.

## Context recipe

- After a search, pass the canonical `id` (preferred) or name to `get_symbol_context` — you get callers, callees, implementors, containment, type references, and any stored annotation in one call.
- For a multi-symbol question (e.g., "compare the three view models"), use `batch_context({ symbols: [...] })` instead of N separate calls.
- Use `get_file_symbols` before `Read`-ing a file: get the symbol layout, then `Read` only the slice you need.

## Tips

- Score-band clustering (`cluster: true`, `cluster_id`, `cluster_page`) is useful when a query returns hundreds of matches — page through `excellent`/`strong`/`possible`/`weak` bands instead of skimming a flat list.
- If snippets look truncated or stale, run `grapha index .` (and `mcp__grapha__reload` if MCP is mounted), then retry — the snippets are stored at index time.
- Only fall back to `Grep`/`Read` after Grapha returns empty or clearly off-target results — and never for a symbol name search.
