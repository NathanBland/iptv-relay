# Troubleshooting

Use these checks when the stack does not start or a client cannot load output.

## Inspect service state

bash
docker-compose ps
docker-compose logs --tail=100 gateway core worker web postgres


If Compose reports a missing variable, set every required value in .env.
Validate the file with docker-compose config --quiet.

## Health check fails

If postgres is unhealthy, check its log and confirm that the configured password matches the initialized database.
If core is unhealthy, check the PostgreSQL connection and the /health/live response.
If web is unhealthy, check the web log and confirm that port 3000 starts inside the container.
If gateway is unhealthy, check the core and web health checks first.

## Port conflict

If another process uses port 8080, set IPTV_GATEWAY_PORT to an unused host port.
Use the same host port in IPTV_PUBLIC_BASE_URL for local clients.
Do not change the internal service ports in Compose.

## No channels

1. Check the source URL.
2. Check the source kind.
3. Check the source status.
4. Inspect the worker log.
5. Check the source timezone for XMLTV data without an offset.
6. Run a manual source sync from the Sources page.

Use UTC when XMLTV values are UTC without an offset.
Do not change the source timezone for values with an explicit offset or Z.

## Jellyfin cannot load output

Use the gateway host name that Jellyfin can resolve.
Keep the output token in the M3U and XMLTV URLs.
Set IPTV_PUBLIC_BASE_URL to the client-facing URL.
Regenerate the setup URLs after an output token rotation.
Check that the reverse proxy permits /out/*.

## LAN access

Keep the gateway behind a TLS reverse proxy when clients connect over a LAN.
Do not send output URLs over untrusted networks without TLS.
Do not expose PostgreSQL port 54329 beyond loopback.

## Test runner failures

Run the default noncredentialed checks:

bash
./scripts/run-test-suite.sh


Use ./scripts/run-test-suite.sh --keep to inspect failed Compose resources.
Use ./scripts/run-test-suite.sh --live only when authorized provider credentials are available.
Do not commit .env.live or other credential files.
