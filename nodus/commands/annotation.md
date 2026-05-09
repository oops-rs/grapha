---
description: Work with Grapha symbol annotations and sync (MCP-first for annotate, CLI for serve/list/sync)
---
Use Grapha annotations instead of dumping notes into ad-hoc markdown files.

## Annotate / read (prefer MCP)

- If the `grapha` MCP server is mounted:
  - Add or replace a symbol note: `mcp__grapha__annotate_symbol({ symbol: "<symbol>", annotation: "<note>", created_by: "claude" })`
  - Read context including annotation: `mcp__grapha__get_symbol_context({ symbol: "<symbol>" })`
- Otherwise, fall back to the CLI:
  - Add or replace: `grapha symbol annotate "<symbol>" "<annotation>" --by claude`
  - Inspect one note: `grapha symbol annotation "<symbol>"`
  - Verify after sync: `grapha symbol context "<symbol>" --fields annotation` or `grapha symbol search "<query>" --fields annotation`

## Serve / list / sync (CLI only — these are daemon and project-management actions)

- Deploy the local LAN annotation service: `grapha annotation serve --port 8080`
- List local annotation records and project identity: `grapha annotation list`
- Sync with another local Grapha annotation service: `grapha annotation sync`
- Override the configured service for one sync: `grapha annotation sync --server http://HOST:8080`

`grapha annotation sync` resolves the service address from `--server`, then `GRAPHA_ANNOTATION_SERVER`, then project `grapha.toml`, then global Grapha config.
Global config can live at `$GRAPHA_CONFIG`, `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml`.
`grapha annotation list` and `grapha annotation sync` use the current directory by default; pass `--path` only when operating on another project.
`grapha annotation serve` is standalone: it does not require an index or a project path, and sync requests carry the project identity used for storage.
Annotation records are project-scoped by default, not branch-scoped; older branch-specific rows remain readable and normalize into the project record.

Record an annotation when the note is compact, factual, and likely to save future agents from rereading files: ownership, business meaning, invariants, migration context, or non-obvious dependencies are good candidates. Avoid recording guesses, obvious restatements of the symbol name, or temporary task scratch.
