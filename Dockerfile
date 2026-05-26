# syntax=docker/dockerfile:1

# ---- Build stage -----------------------------------------------------------
# The Swift bridge links macOS-only libIndexStore.dylib, so Linux containers
# build with the bridge disabled. Rust and the tree-sitter languages work
# fully; Swift falls back to tree-sitter accuracy (no Xcode index store).
FROM rust:1-bookworm AS builder

ENV GRAPHA_SWIFT_BRIDGE_MODE=off \
    CARGO_TERM_COLOR=always

# cmake + pkg-config cover the vendored libgit2 build; SQLite is bundled.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Cache the cargo registry and target dir across builds; copy the binary out
# of the cache mount within the same layer so it persists in the image.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p grapha \
    && cp target/release/grapha /usr/local/bin/grapha

# ---- Runtime stage ---------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates for outbound HTTPS (e.g. annotation/baseline publishing);
# wget powers the container HEALTHCHECK. SQLite and libgit2 are statically
# linked into the binary, so no extra runtime libraries are required.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 grapha

COPY --from=builder /usr/local/bin/grapha /usr/local/bin/grapha
COPY docker/entrypoint.sh /usr/local/bin/grapha-entrypoint
RUN chmod +x /usr/local/bin/grapha-entrypoint

ENV GRAPHA_WORKSPACE=/workspace \
    GRAPHA_HOST=0.0.0.0 \
    GRAPHA_PORT=8080

USER grapha
WORKDIR /workspace
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD wget --quiet --tries=1 --spider "http://127.0.0.1:${GRAPHA_PORT}/api/status" || exit 1

ENTRYPOINT ["grapha-entrypoint"]
