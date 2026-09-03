# Appliance Install

This page documents the pull-only appliance install for CasaOS, Unraid, UGREEN NAS, and generic Docker Compose hosts.

The bundle publishes three application images: `core` and `worker`, `web`, and the `gateway` reverse proxy.
The Caddy configuration is baked into the gateway image, so you do not need `deploy/Caddyfile` on the host.
The appliance Compose file starts the full stack: `gateway`, `web`, `core`, `worker`, and `postgres`.

See [Quick Start](../quick-start.md) for the end-to-end first run.

## Legal notice

You must use only streams that you have the legal right to access.
This project does not provide streams, channels, provider accounts, playlists, programme data, or media content.
You are responsible for the providers, URLs, credentials, and content that you configure.

## Bundle files

The appliance bundle has two files:

- `docker-compose.appliance.yml`
- `.env.appliance.example`

Copy `.env.appliance.example` to `.env` and replace every placeholder value.

## Required variables

| Variable | Required | Description |
|----------|----------|-------------|
| `IPTV_VERSION` | No | Release tag. Default `latest`. Pin a tag for reproducible upgrades. |
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

## Platform install guides

The primary install path on every platform runs the full pull-only Compose stack with `gateway`, `web`, `core`, `worker`, and `postgres`.
The `gateway` and `web` services serve the web UI together.
A single `core` container does not provide the web UI, so the single-container form is unsupported for the full UI.

## CasaOS

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

## Unraid

Unraid supports multi-container Compose through the Compose Manager plugin.
Use the Compose Manager to import and run the appliance Compose bundle.

1. Install the Compose Manager plugin from Unraid Community Applications.
2. Open the Compose Manager and create a new stack.
3. Point the stack at `docker-compose.appliance.yml` and the `.env` file.
4. Start the stack from the Compose Manager.

Open `http://<host-ip>:8080` in a browser to use the web UI.
The single-container Docker tab path is unsupported for the full UI because it cannot run the full service set.

## UGREEN NAS

UGREEN OS supports Docker Compose through its Docker Compose and project UI.
Use the Compose or project UI to import and run the appliance Compose bundle.

1. Open the Docker section and select **Compose** (or **Projects**).
2. Create a new project and upload `docker-compose.appliance.yml`.
3. Add the `.env` file with every required variable replaced.
4. Start the project.

Open `http://<host-ip>:8080` in a browser to use the web UI.
When the UGREEN Docker UI cannot import a multi-container Compose file, use the host terminal to run the appliance Compose bundle.
The single-container image form is unsupported for the full UI.

## Generic Docker Compose

Use this path on any host with Docker Compose.

1. Put `docker-compose.appliance.yml` and `.env` in one folder.
2. Set every required variable in `.env`.
3. Start the stack.

```bash
docker compose -f docker-compose.appliance.yml up -d --wait
```

## First sign-in

Open `http://localhost:8080` in a browser and sign in with the `operator` account.
You can also sign in with the control API when you automate the appliance.

```bash
curl -c cookies.txt http://localhost:8080/api/v1/auth/status
curl -b cookies.txt -c cookies.txt -X POST http://localhost:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{"username": "operator", "password": "your-admin-password"}'
```

The first successful password sign-in permanently disables the bootstrap bearer in the database.

## Add a source

Add an M3U, Xtream, or XMLTV source with the API.

```bash
curl -b cookies.txt -X POST http://localhost:8080/api/v1/sources \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{"kind": "M3U", "name": "My M3U source", "endpoint": "https://example.com/playlist.m3u"}'
```

Set the source `timezone` when an XMLTV source has local timestamps without an offset.
Use an IANA name such as `America/Denver`.
Keep `timezone` as `UTC` when the source uses UTC values without an offset.

## Connect Jellyfin

Read the published Jellyfin setup URLs.

```bash
curl -b cookies.txt http://localhost:8080/api/v1/jellyfin/setup
```

Add the M3U and XMLTV URLs to a Jellyfin M3U tuner.
Set `IPTV_PUBLIC_BASE_URL` to the address that Jellyfin uses to reach the gateway.

## Health checks

Check liveness and readiness after the stack starts.

```bash
curl --fail http://localhost:8080/health/live
curl --fail http://localhost:8080/health/ready
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

## Roll back

A roll back changes only the `IPTV_VERSION` variable back to the previous tag, then recreates the containers.

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
Keep the `.env` file and `IPTV_MASTER_KEY` with the backup.
Without the master key, restored source credentials stay encrypted and unreadable.

Test a restore on a separate PostgreSQL instance before you depend on it.

## Stop and clean up

Stop the services and keep the database volume:

```bash
docker compose -f docker-compose.appliance.yml down
```

Remove the services and database volume only when you want to delete all stored data:

```bash
docker compose -f docker-compose.appliance.yml down --volumes
```
