---
description: Analyze blast radius of changing a symbol (MCP-first, CLI fallback)
---
Use Grapha impact instead of grepping for usages or reading callers by hand.

- If the `grapha` MCP server is mounted, call:
  - `mcp__grapha__get_impact({ symbol: "$ARGUMENTS", depth: 3 })`
- Otherwise, fall back to the CLI:
  - `grapha symbol impact "$ARGUMENTS" --depth 3`

Summarize:
- Direct dependents (depth 1)
- Indirect dependents (depth 2+)
- Whether any entry points are affected

Only fall back to `Grep`/`Read` if the symbol can't be resolved (try `mcp__grapha__search_symbols` / `grapha symbol search` first to find the canonical name).
