---
name: using-grapha
description: "Use when starting any coding conversation — sets the Grapha workflow and points at the right specialist."
---

# Using Grapha

You're doing normal coding work in a repo where Grapha is installed. Grapha is a code intelligence layer that indexes symbols, callers/callees, impact, dataflow, complexity, and durable annotations. Use it as a tool inside your normal workflow, not as a workflow of its own.

Grapha answers code-intelligence questions. FACE (`face`) handles generic grouping, paging, and drill-down for large structured CLI outputs. When using the CLI and a Grapha command returns many JSON records, pipe the records to FACE instead of asking Grapha to solve presentation:

```bash
grapha symbol search "RoomPage" --limit 200 --fields score,file,locator,kind,module \
  | face --by file --within kind
```

## Where Grapha fits

- **Orienting in unfamiliar code.** Don't `Grep` for a function or type name; don't `Read` a whole file to "see what's in it." Ask Grapha for symbols, 360° context, or a file's symbol map — then `Read` only the slice you need. Specialist: `grapha-search`.
- **Reasoning about a change.** Before modifying a public API, refactoring a type, or prioritizing cleanup, check blast radius / complexity / smells. Specialist: `grapha-quality`.
- **Following data.** When the question is "where does this end up?" or "which entry points reach here?", trace it instead of reading code paths by hand. Specialist: `grapha-dataflow`.
- **Persisting or recalling durable knowledge.** Ownership, invariants, business role, or product-term ↔ code mappings — annotate them so the next session doesn't re-derive them. Specialist: `grapha-knowledge`.

Load the matching specialist skill when you reach that step — don't pre-load. If the task crosses several steps, load each as you get there.

## Specialist map

| If your step is… | Load… | It covers |
|---|---|---|
| Find / read / orient | `grapha-search` | `search_symbols`, `get_symbol_context`, `batch_context`, `get_api_surface`, `find_usages`, `get_file_symbols`, `batch_file_symbols`, `get_file_map` |
| Assess change risk and structural health | `grapha-quality` | `get_impact`, `analyze_complexity`, `detect_smells`, `get_module_summary` |
| Trace data through the graph | `grapha-dataflow` | `trace` (forward/reverse), flow entries |
| Persist / read durable code knowledge | `grapha-knowledge` | `annotate_symbol`, annotation serve/list/sync, concept search/bind/alias |

## Access mode

If `mcp__grapha__*` tools are present in the session, use the MCP path. Otherwise use the `grapha` CLI. The tool list tells you which is available — no probe call needed.

## Index freshness

After significant code changes, refresh the index: `mcp__grapha__reload` (MCP) or `grapha index .` (CLI). MCP commonly runs with `--watch` and auto-refreshes. To confirm freshness: `mcp__grapha__get_index_status` (MCP) or `grapha repo status` (CLI).

## Configuration pointers

- Project: `grapha.toml` — `[serve].host`/`[serve].port`/`[serve].watch` for stable serve defaults; `[annotations].server` for sync target; `[repo].name` for non-Git project copies sharing one annotation identity.
- Developer-level defaults: `$GRAPHA_CONFIG`, `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml`.
- Runtime overrides: `GRAPHA_ANNOTATION_SERVER`, or `--server` on individual annotation commands.

## Raw tools are still right when…

…the work isn't about symbols: reading a known path, scanning markdown/JSON/YAML, or grepping a plain string literal (URL, error message, env-var name).
