---
description: Re-index the project with grapha (CLI; reload MCP after)
---
Indexing is CLI-only — the index is written to disk by the `grapha` binary.

- Run `grapha index .` and report the summary (nodes, edges, time).
- If the `grapha` MCP server is also mounted, follow up with `mcp__grapha__reload` so the live MCP server picks up the new index without a restart. (When the server runs as `grapha serve --mcp --watch -p .`, this is automatic — but an explicit reload is safe.)

Use this whenever symbol snippets or relationship queries look stale; indexing refreshes the stored full-symbol snippets used by `mcp__grapha__search_symbols` / `grapha symbol search --context` and `mcp__grapha__get_symbol_context` / `grapha symbol context`.
