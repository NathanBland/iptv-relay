# IPTV Gateway

IPTV Gateway is an MIT-licensed Rust appliance. It turns M3U and Xtream live-TV sources and XMLTV guide data into stable, token-protected endpoints for Jellyfin.

The first successful password sign-in permanently disables the bootstrap bearer in the database.

The gateway exposes M3U, XMLTV, and HDHomeRun-compatible output endpoints.
Provider credentials stay out of output URLs.
Each output URL has an output token in its path.

## Legal notice

You must use only streams that you have the legal right to access.
This project does not provide streams, channels, provider accounts, playlists, programme data, or media content.
You are responsible for the providers, URLs, credentials, and content that you configure.
The maintainers do not control or endorse third-party content.
The maintainers are not responsible for the use of third-party content.

## Quick start for appliance operators

The pull-only appliance bundle uses the signed release images from GitHub Container Registry.
You do not clone the repository and you do not compile code in this path.

The bundle publishes three application images: `core` and `worker`, `web`, and the `gateway` reverse proxy.
The Caddy configuration is baked into the gateway image, so you do not need `deploy/Caddyfile` on the host.
The appliance Compose file starts the full stack: `gateway`, `web`, `core`, `worker`, and `postgres`.
Open the gateway at `http://localhost:8080` and sign in through the web UI.
See the [API overview](#api-overview) and the [Output endpoints](#output-endpoints) sections for the control API and output paths.

### Prerequisites

- Docker Engine and Docker Compose.
- Disk space for the PostgreSQL volume and the provider catalog.
- A release tag from the [GHCR package page](https://github.com/NathanBland/iptv-relay/pkgs/container/iptv-relay).

### Get the bundle files

Download two files into one folder:

- `docker-compose.appliance.yml`
- `.env.appliance.example`

You can fetch them with `curl` from the repository, or copy them from a release archive.

```bash
curl -O https://raw.githubusercontent.com/NathanBland/iptv-relay/main/docker-compose.appliance.yml
curl -O https://raw.githubusercontent.com/NathanBland/iptv-relay/main/.env.appliance.example
cp .env.appliance.example .env
```

### Set the required variables

Open `.env` and replace every placeholder value.
Generate separate random values for the secret variables.

```bash
openssl rand -hex 32
openssl rand -hex 32
openssl rand -base64 32
```

Put the first value in `IPTV_OUTPUT_TOKEN`.
Put the second value in `IPTV_ADMIN_BOOTSTRAP_TOKEN`.
Put the third value in `IPTV_MASTER_KEY`.
Set `POSTGRES_PASSWORD` to a different strong value.
Set `IPTV_ADMIN_PASSWORD` to a password with at least 12 characters.
Set `IPTV_VERSION` to the release tag you selected.

> **Warning:** Do not commit `.env` or live-provider URLs. Keep provider URLs in the local environment file.

### Start the stack

```bash
docker compose -f docker-compose.appliance.yml up -d --wait
```

The stack contains `gateway`, `web`, `core`, `worker`, and `postgres` services.
The `postgres-data` named volume stores the database across container upgrades.

### Check service state

```bash
docker compose -f docker-compose.appliance.yml ps
curl --fail http://localhost:8080/health/live
curl --fail http://localhost:8080/health/ready
```

Open `http://localhost:8080` in a browser to use the web management UI.

### First sign-in

1. Open the gateway at `http://localhost:8080`.
2. Enter the username `operator`.
3. Enter the value from `IPTV_ADMIN_PASSWORD`.
4. Select **Sign in**.

The first successful password sign-in permanently disables the bootstrap bearer in the database.
Keep the administrator password available for later sign-ins.

You can also sign in with the control API when you automate the appliance.

```bash
curl -c cookies.txt http://localhost:8080/api/v1/auth/status
curl -b cookies.txt -c cookies.txt -X POST http://localhost:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{"username": "operator", "password": "your-admin-password"}'
```

The response sets the `iptv_session` and `iptv_csrf` cookies.

### Add the first source

Add an M3U, Xtream, or XMLTV source with the API.

```bash
curl -b cookies.txt -X POST http://localhost:8080/api/v1/sources \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{"kind": "M3U", "name": "My M3U source", "endpoint": "https://example.com/playlist.m3u"}'
```

Wait for the source refresh job to complete.
List channels to confirm that the worker created the catalog.

```bash
curl -b cookies.txt http://localhost:8080/api/v1/channels | head
```

Set the source `timezone` when an XMLTV source has local timestamps without an offset.
Use an IANA name such as `America/Denver`.
Keep `timezone` as `UTC` when the source uses UTC values without an offset.
Do not change the source timezone for values with an explicit offset or `Z`.

### Connect Jellyfin

Read the published Jellyfin setup URLs.

```bash
curl -b cookies.txt http://localhost:8080/api/v1/jellyfin/setup
```

The response returns `playlistUrl`, `xmltvUrl`, and `hdhrDeviceUrl`.
For an M3U tuner, set **File or URL** to `playlistUrl`.
Add `xmltvUrl` as an XMLTV guide provider.
The copied URLs include the output token.
Do not paste provider credentials into Jellyfin.

Set `IPTV_PUBLIC_BASE_URL` to the address that Jellyfin uses to reach the gateway.
Use the gateway host name instead of `localhost` when Jellyfin runs on another host.

For an HDHomeRun tuner, set **Tuner IP Address** to `hdhrDeviceUrl` without the final `/device.xml` path component.
Set `IPTV_TUNER_COUNT` to the number of concurrent tuners that the output profile should report.
See [the Jellyfin configuration guide](docs/docs/configuration/jellyfin.md) for all tuner settings.

## Platform install guides

The primary install path on every platform runs the full pull-only Compose stack with `gateway`, `web`, `core`, `worker`, and `postgres`.
The `gateway` and `web` services serve the web UI together.
A single `core` container does not provide the web UI, so the single-container form is unsupported for the full UI.

### CasaOS

CasaOS does not import multi-container Compose files through its app form.
Use the CasaOS terminal to run the appliance Compose bundle.

1. Open the CasaOS terminal for the host.
2. Install Docker Engine and Docker Compose when they are not present.
3. Download `docker-compose.appliance.yml` and `.env.appliance.example` into one folder.
4. Copy `.env.appliance.example` to `.env` and replace every placeholder value.
5. Start the stack.

```bash
docker compose -f docker-compose.appliance.yml up -d --wait
```

Open `http://<host-ip>:8080` in a browser to use the web UI.
The single-container app form is unsupported for the full UI because it cannot run the full service set.

### Unraid

Unraid supports multi-container Compose through the Compose Manager plugin.
Use the Compose Manager to import and run the appliance Compose bundle.

1. Install the Compose Manager plugin from Unraid Community Applications.
2. Open the Compose Manager and create a new stack.
3. Point the stack at `docker-compose.appliance.yml` and the `.env` file.
4. Start the stack from the Compose Manager.

Open `http://<host-ip>:8080` in a browser to use the web UI.
The single-container Docker tab path is unsupported for the full UI because it cannot run the full service set.

### UGREEN NAS

UGREEN OS supports Docker Compose through its Docker Compose and project UI.
Use the Compose or project UI to import and run the appliance Compose bundle.

1. Open the Docker section and select **Compose** (or **Projects**).
2. Create a new project and upload `docker-compose.appliance.yml`.
3. Add the `.env` file with every required variable replaced.
4. Start the project.

Open `http://<host-ip>:8080` in a browser to use the web UI.
When the UGREEN Docker UI cannot import a multi-container Compose file, use the host terminal to run the appliance Compose bundle.
The single-container image form is unsupported for the full UI.

### Generic Docker Compose

Use this path on any host with Docker Compose.

1. Put `docker-compose.appliance.yml` and `.env` in one folder.
2. Set every required variable in `.env`.
3. Start the stack.

```bash
docker compose -f docker-compose.appliance.yml up -d --wait
```

## Required variables

| Variable | Required | Description |
|----------|----------|-------------|
| `IPTV_VERSION` | No | Release tag, for example `v1.0.0`. Default `latest`. Pin a tag for reproducible upgrades. |
| `POSTGRES_PASSWORD` | Yes | Strong database password. Do not change it after the first start. |
| `IPTV_OUTPUT_TOKEN` | Yes | 64 random hex characters. Protects the output endpoints. |
| `IPTV_ADMIN_BOOTSTRAP_TOKEN` | Yes | 64 random hex characters. Admin bearer, disabled after first sign-in. |
| `IPTV_ADMIN_PASSWORD` | Yes without a hash | 12 or more characters for the `operator` account. |
| `IPTV_ADMIN_PASSWORD_HASH` | No | Argon2id PHC hash. When set, the plaintext password is ignored. |
| `IPTV_MASTER_KEY` | Yes | 32-byte base64 key. Encrypts source credentials at rest. |
| `IPTV_GATEWAY_BIND` | No | Host bind address. Default `127.0.0.1`. |
| `IPTV_GATEWAY_PORT` | No | Host port. Default `8080`. |
| `IPTV_PUBLIC_BASE_URL` | No | Public URL for output endpoints. Default `http://localhost:8080`. |
| `IPTV_TUNER_COUNT` | No | HDHomeRun tuner count. Default `1`. |
| `IPTV_WORKER_COUNT` | No | Worker replicas. Default `4`. |
| `RUST_LOG` | No | Log level filter. Default `info`. |

The `core` and `worker` services must use the same `IPTV_MASTER_KEY`.
The key encrypts source credentials at rest. If you lose the key, encrypted credentials become unreadable.

## Port mapping

The appliance bundle maps the host port `8080` to the gateway container port `8080`.
The gateway proxies `/api/*`, `/auth/*`, `/health/*`, `/metrics`, and `/out/*` to the core service on port `8081`.
The gateway sends all other paths to the web service on port `3000`.

If another process uses port `8080`, set `IPTV_GATEWAY_PORT` to an unused host port.
Use the same host port in `IPTV_PUBLIC_BASE_URL` for local clients.

## Persistent storage

The appliance bundle uses the `postgres-data` named volume.
The volume stores the database across container upgrades and restarts.

When you use a Docker UI that does not support named volumes, map a host directory to `/var/lib/postgresql/data` for the `postgres` service.
Keep the host directory on a persistent disk.

## Health checks

Check liveness and readiness after the stack starts.

```bash
curl --fail http://localhost:8080/health/live
curl --fail http://localhost:8080/health/ready
```

Inspect service logs when a health check fails.

```bash
docker compose -f docker-compose.appliance.yml logs --tail=100 core worker postgres
```

## Upgrade

An upgrade changes only the `IPTV_VERSION` variable, then pulls and recreates the containers.

1. Back up the database.
2. Set `IPTV_VERSION` to the new release tag in `.env`.
3. Pull the new image.
4. Recreate the containers.

```bash
docker compose -f docker-compose.appliance.yml exec -T postgres pg_dump -U iptv -d iptv > iptv-backup.sql
# Edit .env and set IPTV_VERSION to the new tag.
docker compose -f docker-compose.appliance.yml pull
docker compose -f docker-compose.appliance.yml up -d --wait
```

Check `/health/ready` and sign in after the upgrade.
Confirm channels, guide data, and one output stream before normal use.

## Roll back

A roll back changes only the `IPTV_VERSION` variable back to the previous tag, then recreates the containers.

1. Set `IPTV_VERSION` back to the previous release tag in `.env`.
2. Recreate the containers.

```bash
# Edit .env and set IPTV_VERSION back to the previous tag.
docker compose -f docker-compose.appliance.yml up -d --wait
```

Restore the database only when the release requires a database rollback.
Stop `core` and `worker` before you restore a backup.

```bash
docker compose -f docker-compose.appliance.yml stop core worker
cat iptv-backup.sql | docker compose -f docker-compose.appliance.yml exec -T postgres psql -U iptv -d iptv
docker compose -f docker-compose.appliance.yml up -d --wait
```

## Backup and restore

Create a logical backup while the stack runs.

```bash
docker compose -f docker-compose.appliance.yml exec -T postgres pg_dump -U iptv -d iptv > iptv-backup.sql
```

Store the backup outside the host.
Protect backups because they contain gateway configuration and catalog data.
Keep the `.env` file and `IPTV_MASTER_KEY` with the backup.
Without the master key, restored source credentials stay encrypted and unreadable.

Test a restore on a separate PostgreSQL instance before you depend on it.

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
- Network-tuner source records. The refresh worker does not ingest network-tuner sources.
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

The dynamic-event scan creates durable event and filler programmes from provider stream names. The scan applies the built-in sports rules to each enabled event template and uses the template timezone, duration, title, and filler settings. Stored `event_rule_sets` do not drive the scan.

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
| GET | `/api/v1/auth/tokens` | List operator API token metadata. |
| POST | `/api/v1/auth/tokens` | Create an operator API token. The plaintext value is returned once. |
| POST | `/api/v1/auth/tokens/{token_id}/rotate` | Rotate an operator API token. The plaintext value is returned once. |
| POST | `/api/v1/auth/tokens/{token_id}/revoke` | Revoke an operator API token. |

Operator API tokens support `read`, `control`, `output`, and `admin` scopes. The `control` scope also permits read requests. The `admin` scope satisfies every scope. Store each plaintext token securely because the API does not return it again.

### System

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/system` | Return system counts and runtime versions. |
| GET | `/api/v1/support/bundle` | Return a redacted support bundle. Requires admin. |
| GET | `/api/v1/support/logs` | Return recent redacted support log entries. Requires admin. |
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
| GET | `/api/v1/event-templates/suggestions` | List suggested event templates from live stream data. |
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
| POST | `/api/v1/sessions/{provider_pool_id}/{source_id}/{generation}/terminate` | Terminate one active media session. |
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
| GET | `/api/v1/jellyfin/setup` | Return the published Jellyfin URLs. |
| POST | `/api/v1/jellyfin/setup/rotate` | Rotate the output token and return new Jellyfin URLs once. |

## Output endpoints

Output endpoints use a token in the path. Replace `{token}` with the value of `IPTV_OUTPUT_TOKEN`.

At startup, the core stores the token hash in the environment output profile.

The initial profile includes all enabled channels. Output requests use the channel selection and tuner count from this profile.

After an API rotation, the prior token stays valid for the configured overlap window. The default overlap is five minutes.

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/out/{token}/playlist.m3u` | Return the M3U playlist. |
| GET | `/out/{token}/xmltv.xml` | Return the XMLTV guide. |
| GET | `/out/{token}/stream/{*channel_path}` | Stream a channel as MPEG-TS. |
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

Run the standard non-credentialed test suite:

```bash
./scripts/run-test-suite.sh
```

The runner uses an isolated Compose project, removes test resources, and keeps build caches for later runs. Pass `--keep` to inspect resources after a failure. Pass `--live` only when authorized live-provider credentials are available.

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

The full documentation site lives in the `docs/` directory. Preview it locally with MkDocs:

```bash
python3 -m venv docs/.venv
docs/.venv/bin/python -m pip install -r docs/requirements.txt
docs/.venv/bin/mkdocs serve --config-file docs/mkdocs.yml
```

Open the local site at `http://127.0.0.1:8000`.

The generated `docs/site/` directory is local build output. Git ignores this directory.

Use the `iptv-relay` Kaneo project for current tasks and verified feature status.

## License

Original project code is licensed under the [MIT License](LICENSE).
FFmpeg, VLC, container packages, and language dependencies retain their own licenses.
See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for third-party license notices.
