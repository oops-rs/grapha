---
description: Analyze structural complexity of a type (MCP-first, CLI fallback)
---
Use Grapha complexity analysis instead of eyeballing the file with `Read`.

- If the `grapha` MCP server is mounted, call:
  - `mcp__grapha__analyze_complexity({ symbol: "$ARGUMENTS" })`
- Otherwise, fall back to the CLI:
  - `grapha symbol complexity "$ARGUMENTS"`

Summarize the metrics:
- Property count, method count, dependency count
- Invalidation source count (observable properties triggering re-evaluation)
- Init parameter count, extension count
- Blast radius and overall severity rating
