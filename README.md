# IPTV Gateway

IPTV Gateway is an MIT-licensed Rust appliance that turns M3U or Xtream live-TV sources and XMLTV
guide data into stable, token-protected M3U/XMLTV and HDHomeRun-compatible endpoints for Jellyfin.

The implementation is intentionally split into a control plane and a media plane:

- Provider streams are source-scoped assets; channels have stable local UUIDs.
- Multiple viewers of one channel share one upstream provider session.
- Account and shared-pool limits are enforced before a media process or socket starts.
- The live MPEG-TS ring overwrites old chunks without blocking or restarting its upstream.
- Catalog and guide imports stage and validate new generations before activation.

The agent-readable research reference is
[`IPTV_End_to_End_Field_Guide.md`](IPTV_End_to_End_Field_Guide.md). It includes YAML metadata,
stable numbered headings, implementation-study citations, and a source-indexed bibliography.

## Development status

The repository is being built as test-backed vertical slices. The current foundation contains the
Rust workspace, schemas, parser/media primitives, API skeleton, TanStack Start application, and
Docker Compose topology. GitHub Actions image publishing remains deferred until local acceptance
and coverage gates pass.

## Prerequisites

- Rust 1.97.1
- Node 24 and pnpm 11
- FFmpeg and VLC CLI for live adapter tests
- Docker plus `docker-compose`
- `cargo-llvm-cov` for the required Rust coverage gate

## Local commands

```bash
make doctor
make test
make coverage
make ci
docker-compose up --build
```

Copy `.env.example` to an untracked `.env` and replace every development credential before exposing
the service. Live-provider URLs belong only in environment variables; they must never be committed.

## Endpoints

The gateway listens on port 8080. Rust health and control routes are under `/health` and `/api/v1`.
Output-profile endpoints use `/out/{token}/...`; tokens and provider credentials are redacted from
logs and diagnostics.

## License

Original project code is licensed under the [MIT License](LICENSE). FFmpeg, VLC, container packages,
and language dependencies retain their own licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
