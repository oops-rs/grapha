---
description: Work with Grapha symbol annotations and sync
---
Use Grapha's annotation commands based on the requested action:

- To add or replace a symbol note: `grapha symbol annotate "<symbol>" "<annotation>" --by codex`
- To inspect one symbol note: `grapha symbol annotation "<symbol>"`
- To list local annotation records and project/branch identity: `grapha annotation list`
- To deploy the local LAN annotation service: `grapha annotation serve --port 8080`
- To sync with another local Grapha annotation service: `grapha annotation sync`
- To override the configured service for one sync: `grapha annotation sync --server http://HOST:8080`

`grapha annotation sync` resolves the service address from `--server`, then `GRAPHA_ANNOTATION_SERVER`, then project `grapha.toml`, then global Grapha config.
Global config can live at `$GRAPHA_CONFIG`, `$XDG_CONFIG_HOME/grapha/config.toml`, `~/.config/grapha/config.toml`, or `~/.grapha/config.toml`.
`grapha annotation list` and `grapha annotation sync` use the current directory by default; pass `--path` only when operating on another project.
`grapha annotation serve` is standalone: it does not require an index or a project path, and sync requests carry the project identity used for storage.

Record an annotation when the note is compact, factual, and likely to save future agents from rereading files: ownership, business meaning, invariants, migration context, or non-obvious dependencies are good candidates. Avoid recording guesses, obvious restatements of the symbol name, or temporary task scratch.

After syncing, use `grapha symbol context "<symbol>" --fields annotation` or `grapha symbol search "<query>" --fields annotation` to verify that the expected knowledge is available.
