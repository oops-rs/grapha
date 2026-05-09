---
name: using-grapha
description: Orientation for Grapha — when to use it, how to detect MCP, and which specialist skill to load for which task. Load whenever the question touches symbols, callers/callees, impact, dataflow, complexity, smells, repo shape, or stored code knowledge (annotations, concepts).
---

# Using Grapha

Grapha is the first-line code intelligence layer for this repo. Reach for it **before** raw tools (`Read`, `Grep`, `Glob`, `find`, `cat`, `rg`) whenever the question is about symbols rather than raw text.

## Routing policy

- **Grapha-first.** If the question can be phrased in symbol terms (where is `Foo`, who calls `Foo`, what breaks if `Foo` changes, is `Foo` complex, what does `Foo`'s data flow into), route through Grapha. Raw tools are for non-symbol concerns: reading a known path, scanning markdown/JSON/YAML, grepping plain string literals.
- **MCP-first, CLI-fallback.** If the `grapha` MCP server is mounted (look for `mcp__grapha__*` tools in the available/deferred tool list this session), call those — structured JSON, shared running graph, no per-query process spawn. Otherwise fall back to the `grapha` CLI.
- **No probe call needed** to detect MCP — the tool list tells you whether it's mounted.

## Specialist skills (load on demand)

Pick the specialist whose purpose matches the task:

| Task family | Skill | Covers |
|---|---|---|
| Find / read / orient | `grapha-search` | `search_symbols`, `get_symbol_context`, `batch_context`, `get_file_symbols`, `get_file_map` |
| Assess change risk and structural health | `grapha-quality` | `get_impact`, `analyze_complexity`, `detect_smells`, `get_module_summary` |
| Trace data through the graph | `grapha-dataflow` | `trace` (forward/reverse), flow entries |
| Persist / read durable code knowledge | `grapha-knowledge` | `annotate_symbol`, annotation serve/list/sync, concept search/bind/alias |

If a task spans more than one family (e.g., "find the symbol, then check impact, then annotate it"), load each specialist as you reach that step — don't pre-load.

## When raw tools *are* the right answer

- You already know the exact path (often because Grapha just gave it to you) and need to read a specific slice. Use `Read` with `offset`/`limit`.
- You're searching markdown, JSON, YAML, or other non-source text where Grapha has no opinion.
- You're grepping for a plain string literal with no symbol meaning (a hard-coded URL, an error message, an env-var name).

If you're tempted to `Grep` for a function or type name, stop and use `grapha-search` instead — it understands declarations, USRs, modules, and roles.

## Index freshness

The MCP server typically runs as `grapha serve --mcp --watch -p .`, so it auto-refreshes. If results look stale after a large refactor:

- MCP: `mcp__grapha__get_index_status` to confirm, `mcp__grapha__reload` to pick up new index.
- CLI: `grapha repo status` to confirm, `grapha index .` to re-index, then restart the server (or rely on `--watch`).

## Configuration pointers

- Project: `grapha.toml` — set `[serve].host`/`[serve].port`/`[serve].watch` for stable serve defaults; `[annotations].server` for sync target; `[repo].name` for non-Git project copies sharing one annotation identity.
- Developer-level defaults: `$GRAPHA_CONFIG`, `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml`.
- Override at runtime: `GRAPHA_ANNOTATION_SERVER`, or `--server` on individual annotation commands.
