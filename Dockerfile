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
# Cargo.toml or Cargo.lock changes. Limit cook parallelism to one job so the
# heaviest dependency (chrono-tz) compiles alone. This keeps peak memory below
# the local 2 vCPU / 1.9 GiB Docker Desktop limit and prevents SIGKILL during
# cold cache builds. The cached layer makes the slower one-job cook a one-time
# cost that is paid only when the dependency set changes.
RUN CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 cargo chef cook --release -j 1 --recipe-path recipe.json
# Build application binaries. Limit parallelism to two jobs to match the cook
# step and to avoid OOM kills on memory-constrained runners.
COPY . .
RUN CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 cargo build --release --locked -j 2 -p iptv-gateway
# Build the deterministic scale gate binary. The `scale-gate` feature enables
# the fixture generators in `iptv-parsers` and adds no new dependencies.
RUN CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 cargo build --release --locked -j 2 -p iptv-gateway --features scale-gate --bin scale-gate

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg postgresql-client vlc-bin vlc-plugin-base \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 iptv
WORKDIR /app
COPY --from=builder /build/target/release/iptv-gateway /usr/local/bin/iptv-gateway
COPY --from=builder /build/target/release/test-provider /usr/local/bin/test-provider
COPY --from=builder /build/target/release/media-acceptance /usr/local/bin/media-acceptance
COPY --from=builder /build/target/release/fault-acceptance /usr/local/bin/fault-acceptance
COPY --from=builder /build/target/release/live-acceptance /usr/local/bin/live-acceptance
COPY --from=builder /build/target/release/scale-gate /usr/local/bin/scale-gate
COPY tests/fixtures/scale-gate-baseline.json /app/tests/fixtures/scale-gate-baseline.json
COPY LICENSE THIRD_PARTY_NOTICES.md /app/
USER iptv
EXPOSE 8081
STOPSIGNAL SIGTERM
ENTRYPOINT ["iptv-gateway"]
