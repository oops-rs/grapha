---
name: grapha-dataflow
description: Trace data forward from a symbol to terminals (network, persistence, cache, event), or reverse from a symbol back to entry points, using Grapha. Load when reasoning about how data reaches a sink, which entry points exercise a code path, or auditing a flow before changing it. MCP-first; CLI fallback.
---

# Grapha — dataflow tracing

Follow data through the graph instead of hand-walking call chains. **MCP-first** (`mcp__grapha__*`); fall back to the `grapha` CLI.

## Tool map

| Task | MCP tool | CLI fallback |
|---|---|---|
| Forward trace: from symbol to terminals | `mcp__grapha__trace` (`direction: "forward"`) | `grapha flow trace <symbol>` |
| Reverse trace: from symbol back to entry points | `mcp__grapha__trace` (`direction: "reverse"`) | `grapha flow trace <symbol> --direction reverse` |
| List auto-detected entry points | (use search with `role: "entry_point"`) | `grapha flow entries` |

## When to run each

- **Forward trace** when you need to know *what side effects* a symbol's data eventually causes — does this view-model write to the network, the local store, a cache, or fire an event? Useful before adding side-effecting code or auditing a privacy/security flow.
- **Reverse trace** when you need to know *who reaches* a symbol — which screens, jobs, or routes ultimately hit this function? Useful for impact analysis at the entry-point level (more user-facing than `get_impact`'s symbol-graph view).
- **Entry-point listing** when orienting in a large project — what are the user-facing surfaces (SwiftUI Views, `@Observable`s, `fn main`, route handlers)?

## Recipes

- Tight forward trace: `mcp__grapha__trace({ symbol: "<sym>", direction: "forward", depth: 5 })`. Depth defaults to 10 forward / unlimited reverse — start tighter and widen only if you don't see the terminal you expected.
- Reverse-from-terminal: pick a sensitive sink (e.g., the network call), reverse-trace, and the result is the set of entry points that can hit it. Pair with `grapha-quality`'s `get_impact` for a fuller change-risk picture.
- Cluster long traces with `cluster: true` and page through bands.

## Tips

- Terminals are auto-classified (network, persistence, cache, event) — see the project's `grapha.toml` for any user-defined classifier rules.
- If a forward trace ends suspiciously early, the missing edge is often a cross-module call that's ambiguous in tree-sitter mode; a fresh `grapha index .` (then `mcp__grapha__reload`) often resolves it because index-store extraction adds USR-precise edges.
- Forward and reverse traces are *not* symmetric — forward follows data, reverse follows reachability. Pick the one that answers your actual question.
