FROM rust:1.97-bookworm AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY apps/server/Cargo.toml apps/server/Cargo.toml
COPY crates/api/Cargo.toml crates/api/Cargo.toml
COPY crates/domain/Cargo.toml crates/domain/Cargo.toml
COPY crates/ingest/Cargo.toml crates/ingest/Cargo.toml
COPY crates/media/Cargo.toml crates/media/Cargo.toml
COPY crates/parsers/Cargo.toml crates/parsers/Cargo.toml
COPY crates/persistence/Cargo.toml crates/persistence/Cargo.toml
COPY migrations migrations
COPY apps/server/src apps/server/src
COPY crates/api/src crates/api/src
COPY crates/domain/src crates/domain/src
COPY crates/ingest/src crates/ingest/src
COPY crates/media/src crates/media/src
COPY crates/parsers/src crates/parsers/src
COPY crates/persistence/src crates/persistence/src

RUN CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 cargo build --release --locked -p iptv-gateway

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg vlc-bin vlc-plugin-base \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 iptv
WORKDIR /app
COPY --from=builder /build/target/release/iptv-gateway /usr/local/bin/iptv-gateway
COPY --from=builder /build/target/release/test-provider /usr/local/bin/test-provider
COPY --from=builder /build/target/release/media-acceptance /usr/local/bin/media-acceptance
COPY --from=builder /build/target/release/fault-acceptance /usr/local/bin/fault-acceptance
COPY --from=builder /build/target/release/live-acceptance /usr/local/bin/live-acceptance
COPY LICENSE THIRD_PARTY_NOTICES.md /app/
USER iptv
EXPOSE 8081
ENTRYPOINT ["iptv-gateway"]
