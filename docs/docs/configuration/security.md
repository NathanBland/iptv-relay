# Security, Backup, and Recovery

This page documents the security controls, the backup procedure, and the recovery procedure for the gateway.

## Security controls

### Replace every placeholder credential

1. Copy `.env.example` to `.env`.
2. Set `POSTGRES_PASSWORD` to a strong random value.
3. Set `IPTV_OUTPUT_TOKEN` to 64 random hex characters.
4. Set `IPTV_ADMIN_BOOTSTRAP_TOKEN` to 64 random hex characters.
5. Set `IPTV_ADMIN_PASSWORD` to a value with 12 or more characters.
6. Set `IPTV_MASTER_KEY` to a random 32-byte base64 value.

Use a password manager or `openssl rand` to generate each value. Do not reuse a value across variables.

### Prefer a hashed admin password

Set `IPTV_ADMIN_PASSWORD_HASH` to an Argon2id PHC-formatted hash in production. When set, the core service ignores `IPTV_ADMIN_PASSWORD`.

Generate a hash with `argon2`:

```bash
echo -n "your-password" | argon2 somesalt -id -l 32 -t 3 -p 4
```

Put the full PHC string into `IPTV_ADMIN_PASSWORD_HASH`.

### Disable the bootstrap token

The bootstrap bearer token grants full administrator access. The first successful password sign-in disables the token. A restart does not enable the token again.

Do not use the bootstrap token for routine automation. Use a session cookie for later API access.

### Bind the gateway to a local address

The base Compose configuration binds the gateway to `127.0.0.1`. If you expose the gateway on a LAN, put it behind a TLS reverse proxy.

### Rotate the output token

Rotate the environment output token with the API:

```bash
curl -X POST http://localhost:8080/api/v1/jellyfin/setup/rotate \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"overlapSeconds": 300}'
```

The endpoint stores only the SHA-256 hash of the new token. The endpoint returns the plaintext token once. After the overlap window, the old token stops working.

See [Jellyfin](jellyfin.md) for the full rotation behavior.

### Protect the master key

The core and worker services must use the same `IPTV_MASTER_KEY`. The key encrypts source credentials at rest. If you lose the key, encrypted credentials become unreadable.

Store the key in a secret manager. Do not commit the key to the repository.

### Protect proxy logs

Configure the reverse proxy to redact the token segment in every `/out/{token}/...` request path.

Do not send tokenized output paths to an external log service before redaction.

The supplied Caddy files add a restrictive Content-Security-Policy header to the management interface.

The policy allows inline scripts because TanStack Start emits inline hydration scripts during server rendering.

The policy blocks inline event-handler attributes with `script-src-attr 'none'`.

Keep this policy on the web route. Do not apply it to MPEG-TS or XMLTV output routes.

## Backup procedure

### Back up the database

The catalog, guide, sources, and configuration live in PostgreSQL. Back up the database on a schedule.

Run a logical backup with `pg_dump`:

```bash
docker-compose exec -T postgres pg_dump -U iptv iptv > backup-$(date +%F).sql
```

Store the backup file on a separate host. Test the restore on a non-production instance on a regular schedule.

### Back up the environment file

The `.env` file holds every credential. Back up the `.env` file to a secret manager. Do not commit the `.env` file to the repository.

### Back up the master key

Store `IPTV_MASTER_KEY` with the `.env` backup. Without the key, restored source credentials stay encrypted and unreadable.

## Recovery procedure

### Restore the database

1. Stop the core and worker services: `docker-compose stop core worker`.
2. Restore the dump: `docker-compose exec -T postgres psql -U iptv iptv < backup-YYYY-MM-DD.sql`.
3. Start the services: `docker-compose up -d core worker`.

### Recover from a bad reconciliation

List the reconciliation revisions for a source:

```bash
curl http://localhost:8080/api/v1/sources/{source_id}/reconcile/revisions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Roll back to a known-good revision:

```bash
curl -X POST http://localhost:8080/api/v1/sources/{source_id}/reconcile/rollback \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"revision": 3}'
```

The rollback restores channels, streams, and EPG mappings to the target revision.

### Recover the master key

If you lose the master key, restore the key from the secret manager backup. If the key is lost permanently, re-enter every source credential after you set a new key.

### Recover admin access

If the admin password is lost, set a new `IPTV_ADMIN_PASSWORD` in `.env` and restart the core service. If `IPTV_ADMIN_PASSWORD_HASH` is set, replace the hash with a new Argon2id PHC value.
