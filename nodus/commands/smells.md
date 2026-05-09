---
description: Detect code smells across the project graph (MCP-first, CLI fallback)
---
Use Grapha smell detection instead of hand-scanning files.

- If the `grapha` MCP server is mounted, call:
  - `mcp__grapha__detect_smells({})` for the whole repo
  - Add `module`, `file`, or `symbol` arguments to narrow scope (e.g., `{ module: "Room" }`)
- Otherwise, fall back to the CLI:
  - `grapha repo smells $ARGUMENTS`

Present results grouped by severity. Highlight critical smells first, then summarize warning counts by kind.
