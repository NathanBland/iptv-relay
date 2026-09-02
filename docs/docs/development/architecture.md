# Architecture

The gateway is a Rust workspace with a React frontend. The implementation is split into a control plane and a media plane.

## Control plane

The control plane manages catalog and guide data.

- Provider streams are source-scoped assets.
- Channels have stable local UUIDs.
- Catalog and guide imports stage and validate new generations before activation.

The control API runs under `/api/v1`. It uses Axum, SQLx, and PostgreSQL.

## Media plane

The media plane delivers live MPEG-TS streams to viewers.

- Multiple viewers of one channel share one upstream provider session.
- Account and shared-pool limits are enforced before a media process or socket starts.
- The live MPEG-TS ring overwrites old chunks without blocking or restarting its upstream.

The media crate has native HTTP, FFmpeg, and VLC adapters. The current database playback route uses only native HTTP.

## Crate structure

The Rust workspace contains these crates:

| Crate | Description |
|-------|-------------|
| `crates/api` | Axum HTTP API, OpenAPI document, and handlers. |
| `crates/persistence` | SQLx repositories and PostgreSQL migrations. |
| `crates/ingest` | M3U, Xtream, and XMLTV parsers and pipeline. |
| `crates/media` | Media session manager and MPEG-TS ring buffer. |
| `crates/parsers` | M3U, Xtream, and XMLTV parser primitives. |
| `crates/domain` | Shared domain types and utilities. |
| `apps/server` | Core service and worker binary entry points. |
| `apps/web` | React frontend with TanStack Router and TanStack Query. |

## Service topology

The Compose stack runs these services:

| Service | Role |
|---------|------|
| `gateway` | Caddy reverse proxy on port 8080. |
| `web` | React frontend on port 3000. |
| `core` | Axum API and media session manager on port 8081. |
| `worker` | Job worker with configurable replicas. |
| `postgres` | PostgreSQL 17 database. |

The Caddy gateway routes `/api/*`, `/auth/*`, `/health/*`, `/metrics`, and `/out/*` to the core service. It routes all other paths to the web service. It disables buffering for SSE endpoints.

## Data flow

1. A source refresh job downloads and parses the provider playlist or guide.
2. The ingest pipeline stages a new snapshot and activates it in PostgreSQL.
3. Reconciliation merges provider streams into canonical channels by `tvg-id` and normalized name.
4. EPG reconciliation maps canonical channels to EPG channels in three passes.
5. Output endpoints resolve the token to a profile.
6. Output endpoints select enabled channels from that profile.
7. The media plane opens a shared upstream session for each viewer request.

## Migrations

The project uses 27 sequential SQL migrations. A `build.rs` file in `crates/persistence` forces recompilation when migration files change.

All migrations use idempotent SQL constructs such as `CREATE TABLE IF NOT EXISTS` and `ADD COLUMN IF NOT EXISTS`.

## Realtime

The `/api/v1/catalog-events` endpoint publishes `overview` events with system counts every five seconds. The `/api/v1/session-events` endpoint publishes session lifecycle events.

The frontend subscribes to SSE streams from authenticated sessions and invalidates React Query caches when events arrive.
