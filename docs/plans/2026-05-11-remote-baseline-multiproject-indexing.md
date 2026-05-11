# Remote Baseline And Multi-Project Indexing

Date: 2026-05-11

## Purpose

Grapha should answer from the strongest evidence available without pretending that every checkout has the same precision. A local index of the current checkout is authoritative. A published remote default-branch index is a shared baseline when local evidence is missing. External dependency indexes can be merged from local or remote evidence, but every merged symbol must keep enough provenance to explain how fresh and exact the answer is.

## Evidence Ladder

1. Local precise index: a graph built from the current checkout with `grapha index .`. This is the only evidence tier that can claim exact branch truth for local edits, dirty files, and arbitrary feature branches.
2. Local dependency index: a graph already built for an external project and referenced from `grapha.toml`. It is precise for that dependency checkout, but not necessarily for the current repository's branch.
3. Local dependency source: an external project path that Grapha can index during the current run. This is precise for that local source tree and wins over remote evidence.
4. Remote baseline index: a published graph for a project/channel, usually the default branch. This is durable shared evidence, not a claim about the caller's current branch.

Queries should prefer local precise evidence whenever it exists. When a remote baseline contributes symbols, graph nodes carry evidence metadata such as `grapha.evidence.source = "remote_baseline"`, channel, and baseline head fields so later query surfaces can report staleness and confidence.

## Why The Default Branch Is Durable

The default branch is the common reference point for humans, CI, and code review. It changes often, but each published revision has a concrete `head_ref`, `head_oid`, graph version, Grapha version, bundle schema version, and config fingerprint. Keeping that channel durable gives a team one shared baseline without requiring every Grapha server request to clone and index source code.

Release channels are also durable because they represent named product or API states. Branch channels are optional: they are useful during review, but they should be TTL-limited and quota-controlled because branch indexes multiply quickly and are less likely to be reused.

## CI Publishing Flow

The preferred remote update path is CI-generated publishing:

```bash
grapha index .
grapha publish --server http://HOST:8080 --channel default
```

CI checks out the source, builds the local graph, then uploads a bundle containing graph data and revision metadata. The remote Grapha service validates the bundle schema, stores the uploaded graph under `project_id`, rebuilds its own search index from the uploaded graph, and atomically promotes the channel pointer after the bundle and search index are ready.

Remote reindexing by cloning a repository can exist later as an admin-only capability, but it needs explicit repository URL, ref, timeout, auth, and cleanup limits. It is not the default request path.

## Project Identity

`project_id` is the storage and lookup key. It must be stable and collision-resistant because it scopes remote revisions, annotations, and future project metadata.

`repo_name` is not identity. It is a display label, a search/filter field, and the namespace used when merging symbols from multiple repositories:

- `App::RoomPage`
- `FrameUI::GiftBanner`
- `FrameNetwork::RequestClient`

Configured `[repo].name` should shape the human-facing repo label. A configured `[repo].project_id` can pin identity explicitly. Without an explicit project ID, Grapha derives identity from Git remote metadata when available and falls back to a project-path hash.

## External Dependencies

Each `[[external]]` entry can resolve in this priority order:

1. `index_path`: an existing local `.grapha` index or project root with `.grapha/grapha.db`.
2. `path`: a local source tree Grapha can index during the current run.
3. `remote`: a published baseline identified by `server`, `project_id`, and `channel`.

Local dependency evidence wins over remote baseline evidence. Remote dependency symbols keep the configured external repo namespace and are marked as remote-baseline evidence with `head_ref`, `head_oid`, and channel metadata.

## Storage Policy

Remote service storage is revision-oriented:

- `project_id`: canonical project key.
- `repo_name`: display/search namespace.
- `channel`: `default`, `release/*`, or branch-like channels.
- `head_oid` and `head_ref`: exact source revision for the published graph.
- `config_fingerprint`: extraction-relevant config.
- Graph, Grapha, bundle schema, and search schema metadata.

The remote service stores immutable revisions and atomically updates the channel pointer. The `default` channel is durable. `release` and `release/*` channels are durable. All other channels are treated as branch channels and should be subject to TTL and quota cleanup.

## Acceptance Criteria

- `grapha publish --server URL --channel default` uploads the current local graph bundle.
- The remote service accepts compatible bundles, rejects incompatible schemas, rebuilds search from uploaded graph data, and promotes the channel only after storage succeeds.
- Remote graph, search, and project metadata endpoints address data by `project_id`.
- External dependencies prefer local index, then local source, then remote baseline.
- Merged external symbols keep repo namespacing and evidence metadata.
