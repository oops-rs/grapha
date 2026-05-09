# Grapha Workflow

Always-loaded routing rule. Detail lives in skills — load the matching specialist on demand.

## Routing policy

- **Grapha-first.** Use Grapha **before** raw tools (`Read`, `Grep`, `Glob`, `find`, `cat`, `rg`) whenever the question is about symbols, callers/callees, impact, dataflow, complexity, smells, repo shape, or stored code knowledge. Raw tools are for non-symbol concerns (reading a known path, scanning markdown/JSON, grepping plain string literals).
- **MCP-first, CLI-fallback.** If `mcp__grapha__*` tools are present in this session, use them. Otherwise fall back to the `grapha` CLI. No probe call needed — the tool list tells you.

## Skill dispatch

Load `using-grapha` first when in doubt; it explains the model and points at specialists. Otherwise jump straight to:

- **`grapha-search`** — find symbols, read 360° context, list file symbols, orient in modules.
- **`grapha-quality`** — impact, complexity, smells, module summary.
- **`grapha-dataflow`** — forward/reverse trace, entry points.
- **`grapha-knowledge`** — annotations and concept bindings (durable code knowledge).

If the request maps to one of these, load that skill before reaching for raw tools.

## Hard rules

- Never read a whole file to "see what's in it" before asking Grapha. Use `mcp__grapha__get_file_symbols` (CLI: `grapha symbol search --file <path>`) first, then `Read` only the slice you need.
- Never `Grep` for a function or type name. That's `grapha-search`.
- Before modifying any public API, run impact (`grapha-quality`).
- After significant code changes, refresh the index (`grapha index .`) and reload the MCP server (`mcp__grapha__reload`) if mounted.
