FROM rust:1.97-bookworm AS chef
RUN cargo install cargo-chef --locked && rm -rf /usr/local/cargo/registry/cache
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY apps/server/Cargo.toml apps/server/Cargo.toml
COPY crates/api/Cargo.toml crates/api/Cargo.toml
COPY crates/domain/Cargo.toml crates/domain/Cargo.toml
COPY crates/ingest/Cargo.toml crates/ingest/Cargo.toml
COPY crates/media/Cargo.toml crates/media/Cargo.toml
COPY crates/parsers/Cargo.toml crates/parsers/Cargo.toml
COPY crates/persistence/Cargo.toml crates/persistence/Cargo.toml
COPY apps/server/src apps/server/src
COPY crates/api/src crates/api/src
COPY crates/domain/src crates/domain/src
COPY crates/ingest/src crates/ingest/src
COPY crates/media/src crates/media/src
COPY crates/parsers/src crates/parsers/src
COPY crates/persistence/src crates/persistence/src
COPY migrations migrations
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
# Cook dependencies in release mode. This layer stays cached until
# Cargo.toml or Cargo.lock changes. The BuildKit registry mount cache
# avoids re-downloading crates but does not shadow /build/target, so
# the compiled deps live in the Docker layer and are cached by GHA.
# Use -j 4 for CI runners (4 vCPU, 16 GB RAM). Set CARGO_BUILD_JOBS=1
# to override for local Docker Desktop builds with less memory.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
    cargo chef cook --release -j "${CARGO_BUILD_JOBS:-4}" --recipe-path recipe.json
COPY . .
# Build every runtime and test binary in one Cargo invocation. The scale-gate
# feature also builds the normal binaries, so one invocation avoids a second
# full release compilation. The registry mount cache speeds up any crate
# downloads that changed since the cook step. Binaries are copied to /tmp
# so they survive even if a target mount cache is added later.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
    cargo build --release --locked -j "${CARGO_BUILD_JOBS:-4}" -p iptv-gateway --features scale-gate --bins && \
    cp /build/target/release/iptv-gateway /tmp/iptv-gateway && \
    cp /build/target/release/test-provider /tmp/test-provider && \
    cp /build/target/release/media-acceptance /tmp/media-acceptance && \
    cp /build/target/release/fault-acceptance /tmp/fault-acceptance && \
    cp /build/target/release/live-acceptance /tmp/live-acceptance && \
    cp /build/target/release/jellyfin-acceptance /tmp/jellyfin-acceptance && \
    cp /build/target/release/scale-gate /tmp/scale-gate

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg postgresql-client vlc-bin vlc-plugin-base \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 iptv
WORKDIR /app
COPY --from=builder /tmp/iptv-gateway /usr/local/bin/iptv-gateway
COPY --from=builder /tmp/test-provider /usr/local/bin/test-provider
COPY --from=builder /tmp/media-acceptance /usr/local/bin/media-acceptance
COPY --from=builder /tmp/fault-acceptance /usr/local/bin/fault-acceptance
COPY --from=builder /tmp/live-acceptance /usr/local/bin/live-acceptance
COPY --from=builder /tmp/jellyfin-acceptance /usr/local/bin/jellyfin-acceptance
COPY --from=builder /tmp/scale-gate /usr/local/bin/scale-gate
COPY tests/fixtures/scale-gate-baseline.json /app/tests/fixtures/scale-gate-baseline.json
COPY LICENSE THIRD_PARTY_NOTICES.md /app/
USER iptv
EXPOSE 8081
STOPSIGNAL SIGTERM
ENTRYPOINT ["iptv-gateway"]
