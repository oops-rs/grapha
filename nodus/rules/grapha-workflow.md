# Grapha Workflow

- When exploring an unfamiliar part of the codebase, prefer `grapha symbol search` and `grapha symbol context` over reading entire files
- Before modifying any public API, run `grapha symbol impact` to estimate change scope
- Before refactoring a type, run `grapha symbol complexity` to assess structural health
- Use `grapha repo smells` to find code quality issues across the project
- Use `grapha repo modules` to compare module size and coupling before architectural decisions
- After significant code changes, run `grapha index .` to keep the graph fresh and refresh indexed snippets
- Use `grapha repo map` to orient in unfamiliar modules before diving into files
- When searching for a symbol, start with `grapha symbol search` — it's faster and more precise than grep for symbol-level queries
- Use `grapha symbol search --file ...` and `--role ...` before broadening to fuzzy search when a symbol name is too common
- Use `grapha symbol annotate` for durable, reusable symbol notes that should survive future sessions and reduce repeated context loading
- Prefer annotations for expensive-to-rediscover facts: ownership, business role, invariants, dataflow meaning, migration notes, or cross-module coupling
- Do not annotate guesses, obvious symbol names, or temporary task-local scratch context
- Use `grapha annotation serve`, `grapha annotation list`, and `grapha annotation sync` when sharing annotation knowledge across local machines
- Treat `grapha annotation serve` as a standalone annotation daemon; do not require a project index before starting it
- Configure annotation sync with `[annotations].server` in project/global Grapha config, `GRAPHA_ANNOTATION_SERVER`, or an explicit `--server` override
- Configure project graph serving with `[serve].host`, `[serve].port`, and `[serve].watch` when a project should have stable `grapha serve` defaults
- Use `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml` for developer-level Grapha defaults
- Prefer setting `[repo].name` in `grapha.toml` before syncing non-Git project copies that should share the same annotation identity
