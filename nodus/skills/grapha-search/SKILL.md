---
name: grapha-search
description: "Use when work in this repo needs to find symbols, read callers/callees, get 360° context, or list file symbols."
---

# Grapha — search & orient

## Tool map

| Task | MCP tool | CLI fallback |
|---|---|---|
| Find symbols by name / kind / file / role | `mcp__grapha__search_symbols` | `grapha symbol search "<query>" --context` |
| 360° context: callers, callees, impls, annotations | `mcp__grapha__get_symbol_context` | `grapha symbol context <symbol>` |
| Many contexts in one shot | `mcp__grapha__batch_context` | (loop CLI calls) |
| List symbols inside a file | `mcp__grapha__get_file_symbols` | `grapha symbol search --file <path>` |
| List symbols for many known files | `mcp__grapha__batch_file_symbols` | `grapha symbol files <path>...` |
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
- If you already have a plan listing many files, use `batch_file_symbols({ files: [...] })` or `grapha symbol files ...` before doing many individual reads.

## Large CLI result sets

Grapha should answer the code question; FACE should organize the records. For broad searches or file/module result sets, request compact fields from Grapha and pipe the JSON to `face`:

```bash
grapha symbol search "<q>" --limit 200 --fields score,file,locator,kind,module \
  | face --by file --within kind

grapha symbol search "<q>" --limit 200 --fields score,file,locator,kind,module \
  | face --by module --within file --within score --preset bm25
```

Use `face --schema` when you are unsure which fields are available, and save `face --format=json` envelopes when you expect to drill repeatedly.

## Tips

- For MCP-only sessions, use Grapha's `limit`, `fields`, and `cluster` options where available because MCP cannot pipe directly to FACE.
- If snippets look truncated or stale, run `grapha index .` (and `mcp__grapha__reload` if MCP is mounted), then retry — the snippets are stored at index time.
