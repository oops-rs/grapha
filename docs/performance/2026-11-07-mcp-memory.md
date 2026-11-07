# MCP memory investigation and ablations — 2026-11-07

## Observed facts

Native macOS `footprint`, rather than RSS, found seven existing Grapha MCP processes rooted at `/Users/wendell/Developer/WeNext` retaining approximately 10 GB each. One peaked at 15 GB. The persisted index contained 3,938,459 nodes and 6,826,986 edges; SQLite occupied 7.3 GB and the legacy binary cache 5.6 GB. The host had 31,784 MB of swap in use. These are point-in-time observations, not post-installation memory reclamation claims.

The original startup eagerly loaded the complete graph. Watch mode also ran extraction in a background thread and sent entire graphs through an unbounded queue, consumed only when stdin produced another request. Separately, the cache serialized the public graph directly with bincode, although its serde representation omits absent fields and internally tags node roles. A populated-graph round-trip reproduced a decoding failure (`Serde(AnyNotSupported)`); such failures fell back to SQLite and rewrote the cache.

## Constraints and implementation

- Initialization and tool discovery require no graph. Load an existing index on the first tool call; only extract when an index is missing or watch changes require rebuilding.
- A graph is a discardable cache. Evict graph/search state after 60 seconds without a tool call, including when stdin is blocked or non-tool messages keep arriving. Preserve session recall and prune stale symbol IDs after loading.
- Watching an idle project requires no extraction. Coalesce source changes into a capacity-one notification; rebuild once when the next tool call needs the results. Retain invalidation after a failed rebuild so the next call retries.
- Binary serialization must preserve every node and edge field. Use explicit positional records and an untagged binary role representation, streamed through buffered readers/writers. `graph-v2.bincode` and its magic header isolate the format from legacy clients and caches. Legacy cache files are left untouched.

Active queries still materialize the whole graph. Memory therefore scales with workspace size during a query. Loading a large graph again after eviction costs latency; this change prioritizes releasing idle memory without dropping graph data or query results.

## Experiments and ablations

The optimized candidate was built with `cargo build --release -j 2 -p grapha`. The installed baseline and candidate both reported 0.4.2; SHA-256 distinguishes the artifacts:

- Baseline: `b959803c71eb3d869decec36fc9957e98a8100959bbe9c716851c2de93e88dfb`
- Candidate: `cfdc2df9ca7c5a16bb842dd17eea7cfa905685189ab0d7d9cf8986af0552f23c`

An isolated SQLite fixture added 30,000 function nodes with 4,096-byte snippets to a small indexed Rust project. The same JSON-RPC initialize, tools/list, and get_file_map requests were sent to each binary. Memory was measured using `footprint -p PID`, which includes compressed/swapped allocations that RSS misses.

| Phase | Installed baseline | Optimized candidate |
| --- | ---: | ---: |
| Initialize + tool discovery | 254 MB | 3,120 KB |
| First graph query | 254 MB | 135 MB |
| Candidate idle for 63 seconds | — | 19 MB |
| Query after idle | — | 146 MB |

The post-idle primary response content matched the pre-idle content. The versioned cache modification timestamp remained unchanged, proving reload reused the cache instead of rewriting it. An earlier debug build measured 253 MB vs 4,448 KB at initialization, 136 MB after its first query, and 20 MB after eviction. These fixture runs demonstrate the lifecycle effect; they are not a statistical latency benchmark.

A separate initialize + tools/list against the actual WeNext root with `--watch` used 3,712 KB with the optimized candidate. No full candidate query or reindex of that multi-million-node workspace was performed, avoiding another approximately 10 GB materialization on the already pressured host.

Ablation conclusions:

1. **Eager versus lazy loading:** identical initialization requests retain the graph in the baseline and leave it unloaded in the candidate. The live-root initialization drops from roughly 10 GB to 3.6 MB.
2. **Retained versus evicted state:** the candidate's same fixture process drops from 135 MB after querying to 19 MB after idling, then returns equivalent query content after reload. A timer regression also covers connected but silent stdin and repeated initialization traffic.
3. **Snapshot queue versus notifications:** the original queue grows by one graph per pending update. The regression queues 1,000 additional invalidations into a full capacity-one notification channel; none adds a snapshot. The next tool call rebuilds successfully.
4. **Public serde graph versus explicit binary records:** the populated legacy encoding is rejected; versioned records round-trip nodes, all three roles, optional fields, edge provenance, and absent edge fields. Streaming avoids a second buffer proportional to the complete serialized graph. The first-query memory difference combines streaming and cache-format changes and does not isolate their individual contributions.

## Validation and local installation

- `cargo test --workspace -j 2`: 874 passed, 1 ignored, zero failures.
- `cargo clippy --workspace --all-targets -j 2 -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Optimized artifact rebuilt to confirm it was current.
- Installed atomically at `/Users/wendell/.cargo/bin/grapha`; installed SHA-256 matched the candidate. Rollback binary: `/tmp/grapha-before-mcp-memory-20261107`.
- A separate MCP initialize and exact symbol search through the installed binary successfully indexed a temporary project and found its fixture symbol.

Existing MCP processes keep their original executable and allocations until their owning clients reconnect. They were left running to preserve other sessions. These measurements and the local installation preceded the release commit.
