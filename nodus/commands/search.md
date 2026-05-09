---
description: Search project symbols with grapha (MCP-first, CLI fallback)
---
Use Grapha symbol search instead of `Grep`/`Glob` for symbol-level queries.

- If the `grapha` MCP server is mounted (look for `mcp__grapha__search_symbols` in available tools), call:
  - `mcp__grapha__search_symbols({ query: "$ARGUMENTS", fields: "score,id,locator,doc_comment,annotation,signature" })`
  - Retry with `fuzzy: true` if there are no results.
  - Narrow with `kind`, `module`, `file`, `role` before broadening.
- Otherwise, fall back to the CLI:
  - `grapha symbol search "$ARGUMENTS" --context`
  - Retry with `--fuzzy` if no results.
  - Use `--kind`, `--module`, `--file`, and `--role` to narrow noisy matches before falling back to manual file reads.

Present matches with id, kind, file:line, and score. If snippets look stale or truncated, run `grapha index .` (and `mcp__grapha__reload` if MCP is mounted), then retry. Only resort to `Read`/`Grep` after Grapha returns an empty or clearly off-target result.
