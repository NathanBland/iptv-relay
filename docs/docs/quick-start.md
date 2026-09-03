# Quick Start

This guide starts IPTV Gateway with Docker Compose, adds a first source, and connects a client.

## Prerequisites

Install Docker Engine and Docker Compose before you start.
Use a host with enough disk space for the PostgreSQL volume and provider catalog.

## Clone and configure

1. Clone the repository.
2. Change to the repository directory.
3. Copy `.env.example` to `.env`.

```bash
cp .env.example .env
```

Generate separate random values for the three secret variables:

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

!!! warning
    Do not commit `.env` or live-provider URLs. Keep provider URLs in the local environment file.

The default gateway bind is `127.0.0.1`. Keep this value for local use.
Set `IPTV_GATEWAY_BIND` to a LAN address only when a TLS reverse proxy protects the gateway.
Set `IPTV_PUBLIC_BASE_URL` to the HTTPS URL that clients use behind a proxy.

## Start the stack

Run the production-shaped stack in the background:

```bash
docker-compose up --build -d --wait
```

The stack contains `gateway`, `core`, `worker`, `web`, and `postgres` services.
The `postgres-data` volume stores the database across container upgrades.

Check service state:

```bash
docker-compose ps
curl --fail http://localhost:8080/health/live
curl --fail http://localhost:8080/health/ready
```

Open `http://localhost:8080` in a browser.

## First sign-in

1. Enter the username `operator`.
2. Enter the value from `IPTV_ADMIN_PASSWORD`.
3. Select **Sign in**.

The first successful password sign-in permanently disables the bootstrap bearer in the database.
Keep the administrator password available for later sign-ins.

## Add the first source

1. Open the **Sources** page.
2. Select **Add source**.
3. Select `M3U`, `Xtream`, or `XMLTV`.
4. Enter a source name.
5. Enter the provider URL.
6. Save the source.
7. Wait for the source refresh to complete.

Open **Channels** and confirm that the worker created channels.
Use the group filter and search field to check the imported catalog.

### Set the EPG timezone

Set the source timezone when XMLTV timestamps omit an offset.
Use an IANA name such as `America/Denver`.
Use `UTC` when the source provides UTC values without an offset.
Do not change the source timezone when timestamps contain an explicit offset or `Z`.

## Connect Jellyfin

1. Open the **Jellyfin setup** page.
2. Copy the M3U output URL.
3. Copy the XMLTV output URL.
4. Add both URLs to a Jellyfin M3U tuner.
5. Save the tuner and run a guide refresh.

The copied URLs include the output token.
Do not paste provider credentials into Jellyfin.

### Use the HDHomeRun path

Use the tokenized HDHomeRun discovery URL when a client needs an HDHomeRun tuner.
Copy the HDHomeRun device URL from the Jellyfin setup page.
The URL has this form:

```text
http://localhost:8080/out/{token}/hdhr/device.xml
```

Set `IPTV_TUNER_COUNT` to the number of concurrent tuners that the output profile should report.
Use the gateway host name instead of `localhost` when Jellyfin runs on another host.

## Production deployment

Use a stable release or commit for production.
Keep `.env` outside source control and restrict its file permissions.

```bash
chmod 600 .env
docker-compose up --build -d --wait
```

Expose only the gateway port through the TLS reverse proxy.
Do not expose port `54329` to a network.
Keep the database, output token, bootstrap token, and master key backups together.

## Upgrade

1. Back up the database.
2. Fetch the selected release or commit.
3. Review `.env.example` for new variables.
4. Keep existing secret values in `.env`.
5. Rebuild and start the stack.

```bash
docker-compose exec -T postgres pg_dump -U iptv -d iptv > iptv-backup.sql
git fetch --tags
git checkout <release-or-commit>
docker-compose up --build -d --wait
```

Check `/health/ready` and sign in after the upgrade.
Confirm channels, guide data, and one output stream before normal use.

## Roll back

Use the previous release or commit when the new release fails validation.
Keep the current `.env` file and PostgreSQL volume.

```bash
docker-compose down
git checkout <previous-release-or-commit>
docker-compose up --build -d --wait
```

Restore the database only when the release requires a database rollback.
Stop `core` and `worker` before you restore a backup.

```bash
docker-compose stop core worker
cat iptv-backup.sql | docker-compose exec -T postgres psql -U iptv -d iptv
docker-compose up -d --wait
```

## Backup and restore

Create a logical backup while the stack runs:

```bash
docker-compose exec -T postgres pg_dump -U iptv -d iptv > iptv-backup.sql
```

Store the backup outside the repository.
Test a restore on a separate PostgreSQL instance before you depend on it.
Protect backups because they contain gateway configuration and catalog data.

## Troubleshooting

Inspect service logs when a health check fails:

```bash
docker-compose logs --tail=100 gateway core worker web postgres
```

If Compose reports a missing variable, set every required value in `.env` and run `docker-compose config`.
If `core` is unhealthy, check the PostgreSQL log and confirm that `POSTGRES_PASSWORD` did not change after initialization.
If channels are missing, check the source URL, source status, worker log, and source timezone.
If Jellyfin cannot load output, use the gateway host name and keep the output token in both URLs.
If another process uses port 8080, set `IPTV_GATEWAY_PORT` to an unused host port.

## Stop and clean up

Stop the services and keep the database volume:

```bash
docker-compose down
```

Remove the services and database volume only when you want to delete all stored catalog and configuration data:

```bash
docker-compose down --volumes
```

## Next steps

- [Docker Compose](configuration/docker-compose.md): Review the service topology and health checks.
- [Environment Variables](configuration/environment-variables.md): Review all configuration values.
- [Sources](configuration/sources.md): Configure source types and refresh behavior.
- [Jellyfin](configuration/jellyfin.md): Review client output settings.
