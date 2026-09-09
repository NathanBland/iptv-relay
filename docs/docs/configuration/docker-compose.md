# Docker Compose

The default Compose stack runs the gateway, web UI, core API, worker, and PostgreSQL.

## Services

| Service | Image | Port | Description |
|---------|-------|------|-------------|
| `gateway` | `caddy:2.10-alpine` | `8080:8080` | Caddy reverse proxy. Routes `/api/*`, `/auth/*`, `/health/*`, `/metrics`, and `/out/*` to `core`. Routes all other paths to `web`. |
| `web` | Built from `apps/web` | `3000` (internal) | Vite-served React frontend. |
| `core` | Built from `.` | `8081` (internal) | Axum API and media session manager. |
| `worker` | Built from `.` | _none_ | Job worker. Default replica count is 4. |
| `postgres` | `postgres:17-alpine` | `127.0.0.1:54329:5432` | PostgreSQL database. |

The `gateway` service depends on `core` and `web`. The `core` and `worker` services depend on `postgres`.

## Volumes

| Volume | Mount | Description |
|--------|-------|-------------|
| `postgres-data` | `/var/lib/postgresql/data` | PostgreSQL data directory. |

The Compose file mounts `./deploy/Caddyfile` read-only into the `gateway` container at `/etc/caddy/Caddyfile`.

## Health checks

| Service | Check | Interval | Timeout | Retries |
|---------|-------|----------|---------|---------|
| `web` | `fetch('http://127.0.0.1:3000/')` | 10s | 5s | 12 |
| `core` | `curl --fail http://127.0.0.1:8081/health/live` | 10s | 5s | 12 |
| `postgres` | `pg_isready -U iptv -d iptv` | 5s | 5s | 20 |

## Test profile services

The Compose file defines a `test` profile with extra services:

| Service | Description |
|---------|-------------|
| `fake-provider` | Test MPEG-TS provider for acceptance tests. |
| `toxiproxy` | Network fault injection proxy. |
| `media-acceptance` | Deterministic media acceptance test. |
| `fault-acceptance` | Fault-injection acceptance test. |
| `live-acceptance` | Live-provider acceptance test. |
| `jellyfin-acceptance` | Deterministic Jellyfin-output acceptance test. |

The real-provider Jellyfin gate runs from the host with `make live-jellyfin-acceptance`.
It uses a disposable Jellyfin server and reads `.env.live` and `.env.xtreme` without changing either file.

Run a test service with the `--profile test` flag.

## Caddy gateway

The Caddy gateway listens on port 8080. It disables buffering for SSE endpoints with `flush_interval -1`. It compresses responses with zstd and gzip.

The SSE matcher covers `/api/v1/session-events` and `/api/v1/catalog-events`.

## Commands

Start the stack:

```bash
docker-compose up --build
```

Stop the stack:

```bash
docker-compose down
```

Validate the Compose configuration:

```bash
make compose-config
```

## Dev mode

The `docker-compose.dev.yml` override adds hot reload for Rust and web development.

Build the dev images:

```bash
make compose-dev-build
```

Start the dev environment:

```bash
make compose-dev-up
```

Follow the logs:

```bash
make compose-dev-logs
```

Stop the dev environment:

```bash
make compose-dev-down
```

The dev `Dockerfile.dev` uses `cargo-chef` to cook dependencies at build time. A polling script detects source changes and triggers incremental rebuilds.
