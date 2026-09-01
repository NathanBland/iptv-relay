# IPTV Gateway

IPTV Gateway is an MIT-licensed Rust appliance. It turns M3U and Xtream live-TV sources and XMLTV guide data into stable, token-protected endpoints for Jellyfin.

The first successful password sign-in permanently disables the bootstrap bearer in the database.

The gateway exposes M3U, XMLTV, and HDHomeRun-compatible output endpoints.
Provider credentials stay out of output URLs.
Each output URL has an output token in its path.

## Architecture

The implementation is split into a control plane and a media plane.

**Control plane:**

- Provider streams are source-scoped assets.
- Channels have stable local UUIDs.
- Catalog and guide imports stage and validate new generations before activation.

**Media plane:**

- Multiple viewers of one channel share one upstream provider session.
- The media crate has account and shared-pool limit support.
- The live MPEG-TS ring overwrites old chunks without blocking or restarting its upstream.

The HTTP output route applies database provider limits and ordered alternate streams. It does not apply stored stream profiles or process-adapter selections.

The agent-readable research reference is [`IPTV_End_to_End_Field_Guide.md`](IPTV_End_to_End_Field_Guide.md). It includes YAML metadata, stable numbered headings, and a source-indexed bibliography.

## Features

### Sources and ingestion

- M3U playlist ingestion.
- Xtream Codes live-stream ingestion.
- XMLTV EPG ingestion.
- Configurable source refresh intervals.
- Manual source sync.

### Channels and groups

- Canonical channel reconciliation by `tvg-id` and normalized name.
- Channel enable and disable.
- Group enable and disable with bulk operations.
- Server-side channel and group search.
- Stream preview with `mpegts.js`.

### EPG

- Automatic EPG mapping by `tvg-id` and normalized name.
- EPG mapping review queue.
- Manual EPG mapping overrides.
- Reconciliation trigger endpoint.
- TV guide page.

XMLTV times use UTC internally. Set the source timezone when XMLTV values omit an offset.
An explicit offset or `Z` marker takes precedence over the source timezone.
The M3U source does not provide programme times, so its timezone does not shift channel metadata.
The guide stores each normalized instant in UTC and retains the original XMLTV value.
The TV Guide page shows the current programme first and formats times in the browser timezone.

### Events and lineups

- Event-template records and scan endpoints.
- Event-channel records and prune endpoints.
- Lineup templates with categories, channels, and aliases.
- Lineup apply workflow.

### Stream health and profiles

- Stream-health metadata and status APIs.
- Stored quality ranks for channel streams.
- Stream-profile records and channel assignments.

### Access and realtime

- Admin authentication with CSRF protection.
- User, role, and grant data records.
- Channel name standardization with aliases.
- Realtime SSE catalog and session events.

### DVR and output

- Recording-rule and recording metadata APIs.
- HDHomeRun emulation.
- M3U and XMLTV output endpoints.
- Jellyfin integration.

The v1 runtime does not execute stream profiles, probe streams with `ffprobe`, enforce multi-user grants, or record media files.

The dynamic-event scan creates durable event and filler programmes for legacy templates and built-in sports rules.
Stored `event_rule_sets` and per-template filler or category settings do not drive the scan yet.

## Quick start

### Prerequisites

- Docker and Docker Compose.
- A copy of `.env.example` renamed to `.env`.

### Start the stack

1. Copy `.env.example` to `.env`.
2. Replace every placeholder credential in `.env`.
3. Run the stack:

```bash
docker-compose up --build
```

4. Open the gateway at `http://localhost:8080`.
5. Sign in with the username `operator` and the password from `IPTV_ADMIN_PASSWORD`.

### Add a source

1. Open the Sources page.
2. Select **Add source**.
3. Enter the source type, name, and URL.
4. Save the source.
5. Wait for the refresh job to complete.

Set `timezone` when an XMLTV source has local timestamps without an offset.
Use an IANA name such as `America/Denver`.
Keep `timezone` as `UTC` when the source uses UTC values without an offset.
Do not change the source timezone for values with an explicit offset or `Z`.

### Configure Jellyfin

1. Open the Jellyfin setup page.
2. Copy the M3U and XMLTV output URLs.
3. Add the URLs to Jellyfin as an M3U tuner.
4. Use URLs that contain the `IPTV_OUTPUT_TOKEN` value.

## Configuration

The gateway reads configuration from environment variables. The Compose file passes these variables to the `core` and `worker` services.

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | Generated | Compose creates this value from `POSTGRES_PASSWORD`. |
| `IPTV_GATEWAY_BIND` | `127.0.0.1` | Host address for the gateway port. |
| `IPTV_GATEWAY_PORT` | `8080` | Host port for the gateway. |
| `IPTV_BIND` | `0.0.0.0:8081` | Core service bind address. |
| `IPTV_PUBLIC_BASE_URL` | `http://localhost:8080` | Public gateway base URL for output endpoints. |
| `IPTV_OUTPUT_TOKEN` | Required | Random 256-bit token that protects output endpoints. |
| `IPTV_ADMIN_BOOTSTRAP_TOKEN` | Required | Random 256-bit bearer token for bootstrap admin access. |
| `IPTV_ADMIN_PASSWORD` | Required without a hash | Password with at least 12 characters for the `operator` account. |
| `IPTV_ADMIN_PASSWORD_HASH` | _empty_ | Argon2id PHC hash. When set, the plaintext password is ignored. |
| `IPTV_MASTER_KEY` | Required | Base64 32-byte key for secret encryption. |
| `IPTV_TUNER_COUNT` | `1` | HDHomeRun tuner count for the environment output profile. |
| `IPTV_WORKER_COUNT` | `4` | Number of worker replicas. |
| `IPTV_POSTGRES_PORT` | `54329` | Host port mapped to PostgreSQL. |
| `POSTGRES_PASSWORD` | Required | PostgreSQL password. |
| `RUST_LOG` | `info,iptv_gateway=debug` | Log level filter. |

Do not commit live-provider URLs. Put live-provider URLs only in environment variables.

The base Compose configuration binds the gateway to `127.0.0.1`.

If you expose the gateway on a LAN, put it behind a TLS reverse proxy.

## Development

### Local setup

Install these tools:

- Rust 1.97.1.
- Node 24 and pnpm 11.
- FFmpeg and VLC CLI for live adapter tests.
- `cargo-llvm-cov` for the Rust coverage gate.

Run the doctor check:

```bash
make doctor
```

Start PostgreSQL with Compose:

```bash
make postgres-test
```

Run the Rust and web tests:

```bash
make test
```

### Docker dev mode

The dev override adds hot reload for Rust and web development.

Build the dev images:

```bash
make compose-dev-build
```

Start the dev environment:

```bash
make compose-dev-up
```

Follow the core, worker, and web logs:

```bash
make compose-dev-logs
```

Stop the dev environment:

```bash
make compose-dev-down
```

## API overview

The gateway listens on port 8080.
The Caddy gateway proxies `/api/*`, `/auth/*`, `/health/*`, `/metrics`, and `/out/*` to core port 8081.
The gateway sends all other paths to web port 3000.

### Authentication

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/auth/status` | Return the current session status. |
| POST | `/api/v1/auth/login` | Start an admin session. |
| POST | `/api/v1/auth/logout` | End the current session. |

### System

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/system` | Return system counts and runtime versions. |
| GET | `/api/v1/settings/schema` | Return the settings schema. |
| GET | `/api/v1/openapi.json` | Return the OpenAPI document. |
| GET | `/health/live` | Return liveness status. |
| GET | `/health/ready` | Return readiness status. |
| GET | `/metrics` | Return Prometheus metrics. |

### Sources

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/sources` | List configured sources. |
| POST | `/api/v1/sources` | Create a source. |
| PATCH | `/api/v1/sources/{source_id}` | Update a source. |
| DELETE | `/api/v1/sources/{source_id}` | Delete a source and related data. |
| PATCH | `/api/v1/sources/{source_id}/refresh-interval` | Set the refresh interval. |
| POST | `/api/v1/sources/{source_id}/sync` | Trigger a manual source sync. |

### Channels and groups

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/channels` | List channels with pagination and search. |
| POST | `/api/v1/channels` | Create a channel. |
| GET | `/api/v1/channels/{channel_id}/preview` | Return the preview stream URL. |
| GET | `/api/v1/channels/{channel_id}/stream` | Proxy the MPEG-TS stream. |
| PATCH | `/api/v1/channels/{channel_id}/enabled` | Enable or disable a channel. |
| GET | `/api/v1/channels/{channel_id}/best-stream` | Return a stored stream selection. |
| POST | `/api/v1/channels/{channel_id}/stream-profile` | Store a stream-profile assignment. |
| DELETE | `/api/v1/channels/{channel_id}/stream-profile` | Remove a stored stream-profile assignment. |
| GET | `/api/v1/groups` | List groups with channel counts. |
| PATCH | `/api/v1/groups/{group_name}/enabled` | Enable or disable a group. |
| PATCH | `/api/v1/groups/enabled` | Enable or disable all groups. |

### EPG

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/programmes` | List programmes with pagination and search. |
| POST | `/api/v1/epg/reconcile` | Trigger EPG reconciliation. |
| GET | `/api/v1/epg/mappings` | List EPG mappings. |
| GET | `/api/v1/epg/unmapped` | List channels with no EPG mapping. |
| GET | `/api/v1/epg/review/{channel_id}/candidates` | List review candidates. |
| GET | `/api/v1/epg/channels/search` | Search EPG channels. |
| PATCH | `/api/v1/channels/{channel_id}/epg-mapping` | Set a manual EPG mapping. |
| DELETE | `/api/v1/channels/{channel_id}/epg-mapping` | Remove an EPG mapping. |
| POST | `/api/v1/epg/review/{channel_id}/resolve` | Accept or reject a review candidate. |

### Events

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/events` | List stored event records. |
| GET | `/api/v1/event-templates` | List event templates. |
| POST | `/api/v1/event-templates` | Create an event template. |
| PATCH | `/api/v1/event-templates/{template_id}` | Update an event template. |
| DELETE | `/api/v1/event-templates/{template_id}` | Delete an event template. |
| GET | `/api/v1/event-channels` | List event channels. |
| POST | `/api/v1/event-templates/{template_id}/scan` | Store matching provider-stream records. |
| POST | `/api/v1/event-templates/{template_id}/prune` | Hide old event-channel records. |

### Lineups

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/lineup-templates` | List lineup templates. |
| POST | `/api/v1/lineup-templates` | Create a lineup template. |
| DELETE | `/api/v1/lineup-templates/{template_id}` | Delete a lineup template. |
| GET | `/api/v1/lineup-templates/{template_id}/categories` | List lineup categories. |
| GET | `/api/v1/lineup-templates/{template_id}/channels` | List lineup channels. |
| POST | `/api/v1/lineup-templates/{template_id}/apply` | Apply a lineup template. |

### Sessions and realtime

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/sessions` | List active media sessions. |
| GET | `/api/v1/session-events` | Subscribe to session SSE events. |
| GET | `/api/v1/catalog-events` | Subscribe to catalog SSE events. |

### Stream health

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/streams/health` | List stream health data. |
| GET | `/api/v1/streams/health/stats` | Return aggregate health statistics. |
| POST | `/api/v1/streams/health/check` | Mark candidate streams as `checking`. |
| POST | `/api/v1/streams/rank` | Rank all channel streams by quality. |

### Users

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/users` | List user accounts. |
| POST | `/api/v1/users` | Create a user account. |
| PATCH | `/api/v1/users/{user_id}` | Update a user account. |
| DELETE | `/api/v1/users/{user_id}` | Delete a user account. |

### Channel aliases

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/channel-aliases` | List channel aliases. |
| POST | `/api/v1/channel-aliases` | Create a channel alias. |
| DELETE | `/api/v1/channel-aliases/{alias_id}` | Delete a channel alias. |
| GET | `/api/v1/channel-aliases/resolve` | Resolve a name to its canonical form. |

### Recordings

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/recordings/rules` | List recording-rule records. |
| POST | `/api/v1/recordings/rules` | Create a recording-rule record. |
| DELETE | `/api/v1/recordings/rules/{rule_id}` | Delete a recording-rule record. |
| GET | `/api/v1/recordings` | List recording metadata records. |
| POST | `/api/v1/recordings` | Create a recording metadata record. |
| DELETE | `/api/v1/recordings/{recording_id}` | Delete a recording metadata record. |
| GET | `/api/v1/recordings/stats` | Return stored recording counts. |

### Stream profiles

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/stream-profiles` | List stream profiles. |
| POST | `/api/v1/stream-profiles` | Create a stream-profile record. |
| DELETE | `/api/v1/stream-profiles/{profile_id}` | Delete a stream profile. |

### Jobs and Jellyfin

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/jobs` | List jobs. |
| POST | `/api/v1/jobs/{job_id}/cancel` | Cancel a job. |
| PUT | `/api/v1/jellyfin` | Save the Jellyfin configuration. |

## Output endpoints

Output endpoints use a token in the path. Replace `{token}` with the value of `IPTV_OUTPUT_TOKEN`.

At startup, the core stores the token hash in the environment output profile.

The initial profile includes all enabled channels. Output requests use the channel selection and tuner count from this profile.

After a token change, restart the core. The prior token stays valid for five minutes.

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/out/{token}/playlist.m3u` | Return the M3U playlist. |
| GET | `/out/{token}/xmltv.xml` | Return the XMLTV guide. |
| GET | `/out/{token}/stream/{channel_path}` | Stream a channel as MPEG-TS. |
| GET | `/out/{token}/hdhr/discover.json` | Return the HDHomeRun discovery document. |
| GET | `/out/{token}/hdhr/lineup.json` | Return the HDHomeRun lineup. |
| GET | `/out/{token}/hdhr/lineup_status.json` | Return the HDHomeRun lineup status. |
| GET | `/out/{token}/hdhr/device.xml` | Return the HDHomeRun device XML. |

The current UI does not configure output-profile channel selections.

## Testing

Run the doctor check before tests:

```bash
make doctor
```

Run Rust and web tests:

```bash
make test
```

Run Rust and web tests in parallel:

```bash
make test-parallel
```

Run integration tests against PostgreSQL:

```bash
make test-integration
```

Run end-to-end tests:

```bash
make test-e2e
```

Run end-to-end tests with real sources:

```bash
make test-e2e-real
```

Run the coverage gate:

```bash
make coverage
```

Run the changed-line coverage gate:

```bash
make coverage-changed
```

Run the full CI pipeline:

```bash
make ci
```

Run the media acceptance test:

```bash
make media-acceptance
```

Run the fault-injection acceptance test:

```bash
make fault-acceptance
```

Run the live-provider acceptance test:

```bash
make live-acceptance
```

## Documentation

The full documentation site lives in the `docs/` directory. Build and serve it locally with MkDocs:

```bash
cd docs
pip install -r requirements.txt
mkdocs serve
```

Open the local site at `http://127.0.0.1:8000`.

Use the `iptv-relay` Kaneo project for current tasks and verified feature status.

## License

Original project code is licensed under the [MIT License](LICENSE).
FFmpeg, VLC, container packages, and language dependencies retain their own licenses.
See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for third-party license notices.
