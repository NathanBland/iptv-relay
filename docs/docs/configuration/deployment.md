# Source Build Deployment

This page documents a source-build deployment for developers who build the images from a repository checkout.
For the pull-only appliance path on production hosts, use the [Appliance Install](appliance-install.md) bundle instead.

Use Docker Compose for a single-host source-build deployment.

## Requirements

Install Docker Engine, Docker Compose, OpenSSL, cURL, and Git.
Use a stable release or commit.
Provide persistent storage for the postgres-data volume.

## Configure secrets

1. Copy .env.example to .env.
2. Generate separate values with openssl rand.
3. Set all required values in .env.
4. Set IPTV_PUBLIC_BASE_URL to the client-facing HTTPS URL.
5. Set IPTV_GATEWAY_BIND to the local host address.
6. Restrict .env permissions.

```bash
cp .env.example .env
chmod 600 .env
openssl rand -hex 32
openssl rand -hex 32
openssl rand -base64 32
```


Use the values for IPTV_OUTPUT_TOKEN, IPTV_ADMIN_BOOTSTRAP_TOKEN, and IPTV_MASTER_KEY.
Use a separate strong value for POSTGRES_PASSWORD.
Use IPTV_ADMIN_PASSWORD_HASH with an Argon2id PHC value when possible.

## Start and validate

```bash
docker-compose config --quiet
docker-compose up --build -d --wait
docker-compose ps
curl --fail https://gateway.example.com/health/live
curl --fail https://gateway.example.com/health/ready
```


Expose the gateway through a TLS reverse proxy.
Do not expose the PostgreSQL port to a network.
Keep the default gateway bind on loopback when the proxy runs on the same host.

## Upgrade

1. Back up PostgreSQL.
2. Fetch the target release.
3. Review new variables in .env.example.
4. Keep existing secret values.
5. Rebuild and start the stack.
6. Check health and one output stream.

```bash
docker-compose exec -T postgres pg_dump -U iptv -d iptv > iptv-backup.sql
git fetch --tags
git checkout <release-or-commit>
docker-compose up --build -d --wait
```


## Roll back

1. Stop the stack.
2. Check out the previous release.
3. Keep .env and the postgres-data volume.
4. Start the stack.
5. Restore PostgreSQL only when the release requires it.

```bash
docker-compose down
git checkout <previous-release-or-commit>
docker-compose up --build -d --wait
```


## Backup and restore

Create a logical backup:

```bash
docker-compose exec -T postgres pg_dump -U iptv -d iptv > iptv-backup.sql
```


Stop application services before a restore:

```bash
docker-compose stop core worker
cat iptv-backup.sql | docker-compose exec -T postgres psql -U iptv -d iptv
docker-compose up -d --wait
```


Store backups outside the repository.
Protect backups because they contain gateway configuration and catalog data.
Test each restore on a separate PostgreSQL instance.

## State and scaling

Run one `core` replica per deployment.
The core process owns the live media-session registry and provider-slot leases.
Keep all worker replicas connected to the same PostgreSQL database.
PostgreSQL stores jobs, leases, channel identity, source snapshots, and recovery state.

Do not run multiple core replicas behind a load balancer.
Multiple core replicas would keep separate session registries and could route termination to the wrong process.
Add a shared session registry before you scale core horizontally.

A core restart closes its local media sessions.
Jellyfin reconnects through the catalog after the new core becomes healthy.
Worker restarts recover queued and leased jobs from PostgreSQL.
Concurrent session termination is idempotent at the core process.

Valkey is not required for the supported single-core Compose topology.
Re-evaluate this decision when core replicas or cross-host session ownership become deployment requirements.

## Cleanup

Stop services and keep data:

```bash
docker-compose down
```


Delete services and all database data only when you no longer need the data:

```bash
docker-compose down --volumes
```


## Repository checks

Run the standard noncredentialed test command from the repository root:

```bash
./scripts/run-test-suite.sh
```


Use --keep to retain Compose resources after a failure.
Use --live only with authorized live-provider credentials.
The runner uses retained build caches to reduce later build time.
