---
name: grapha-knowledge
description: Persist and recall durable code knowledge through Grapha — symbol annotations (ownership, invariants, business role) and concept bindings (business term → code). Load when capturing reusable facts you don't want future agents to re-derive, or when resolving a product/business term to its code surface. MCP-first for read/write of records; CLI for the annotation daemon and sync.
---

# Grapha — durable knowledge

Two persistent layers Grapha maintains on top of the graph:

- **Annotations** — short, agent-written notes attached to a symbol by ID/USR. Future agents read them via `get_symbol_context` and can save the cost of re-deriving the same context.
- **Concepts** — business/product terms bound to one or more code symbols, with aliases. Lets agents resolve "the room invitation flow" to actual symbols without rediscovering them every session.

**MCP-first** (`mcp__grapha__*`) for the record-level read/write tools; **CLI** for the annotation service daemon and sync.

## Annotation tool map

| Task | MCP tool | CLI fallback |
|---|---|---|
| Add or replace an annotation | `mcp__grapha__annotate_symbol` | `grapha symbol annotate "<symbol>" "<note>" --by <agent>` |
| Read context including annotation | `mcp__grapha__get_symbol_context` | `grapha symbol context "<symbol>"` |
| Inspect one note (CLI-only short form) | — | `grapha symbol annotation "<symbol>"` |
| Verify after sync | `mcp__grapha__get_symbol_context` (look at `annotation` field) | `grapha symbol context "<symbol>" --fields annotation` |

### Annotation daemon and sync (CLI only)

These are project-management actions, not query tools:

- `grapha annotation serve --port 8080` — standalone local LAN annotation service. Does **not** require `grapha index` or a project path.
- `grapha annotation list` — local annotation records and project identity.
- `grapha annotation sync` — pull/merge/push against the configured service.
- `grapha annotation sync --server http://HOST:8080` — one-off override.

Sync resolves the service address from `--server`, then `GRAPHA_ANNOTATION_SERVER`, then project `grapha.toml`, then global Grapha config. Records are project-scoped by default; older branch-specific rows remain readable. Set `[repo].name` in `grapha.toml` when syncing non-Git copies that should share one project identity.

### When to record an annotation

Compact, factual, expensive-to-rediscover. Good candidates:

- Ownership and business role.
- Invariants the type promises but doesn't enforce in code.
- Cross-module coupling that isn't visible from one file.
- Migration state ("this is the new path; old `XLegacy` is being removed in 2026Q3").
- Non-obvious dependencies or side effects.

**Don't** annotate guesses, obvious restatements of the symbol name, or task-local scratch.

## Concept tool map

| Task | MCP tool | CLI fallback |
|---|---|---|
| Resolve a business term to code scopes | `mcp__grapha__search_concepts` | `grapha concept search "<term>"` |
| Inspect a stored concept | `mcp__grapha__get_concept` | `grapha concept get "<term>"` |
| Bind a concept to one or more symbols | `mcp__grapha__bind_concept` | `grapha concept bind "<term>" <symbols...>` |
| Add aliases to an existing concept | `mcp__grapha__add_concept_alias` | `grapha concept alias "<term>" <aliases...>` |
| Remove a concept | `mcp__grapha__remove_concept` | `grapha concept remove "<term>"` |

### When to bind a concept

When the user uses a domain term (e.g., "room invitation", "checkout funnel", "audit log") and you've identified the symbols that implement it, bind once so the next session can resolve the term in a single call instead of re-searching. Add aliases for common variants the team uses.

`search_concepts` falls back through stored bindings → localization → assets → symbol heuristics, so even an un-bound term often resolves usefully — but a binding is still the cheapest path.

## Tips

- Annotations are keyed by Grapha ID / Swift USR, so they survive renames at the language level (the ID tracks the declaration).
- Concept bindings are *additive* — `bind_concept` with new symbols extends the binding rather than replacing it. To replace, remove and rebind.
- After bulk imports of annotations or concepts, run `grapha annotation list` (or for concepts, search-then-get) to confirm shape before relying on the data.
