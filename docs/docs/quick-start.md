# Quick Start

This guide starts the gateway with Docker Compose and adds a first source.

## Prerequisites

Install these tools before you start:

- Docker and Docker Compose.

## Clone and configure

1. Clone the repository.
2. Copy `.env.example` to `.env`.
3. Replace every placeholder credential in `.env`.
4. Set a unique database password.
5. Set a random 64-character hexadecimal value for `IPTV_OUTPUT_TOKEN`.
6. Set a different random 64-character hexadecimal value for `IPTV_ADMIN_BOOTSTRAP_TOKEN`.
7. Set an administrator password with at least 12 characters.
8. Set a random 32-byte base64 value for `IPTV_MASTER_KEY`.

!!! warning
    Do not commit live-provider URLs. Put live-provider URLs only in environment variables.

## Start the stack

Build and start all services:

```bash
docker-compose up --build
```

Wait for the `core`, `web`, and `postgres` health checks to pass.

Open the gateway at `http://localhost:8080`.

## Log in

1. Open `http://localhost:8080` in a browser.
2. Enter the username `operator`.
3. Enter the password from `IPTV_ADMIN_PASSWORD`.
4. Select **Sign in**.

The management interface loads the overview page.

## Add a source

1. Open the **Sources** page.
2. Select **Add source**.
3. Choose the source type: M3U, Xtream, or XMLTV.
4. Enter the source name.
5. Enter the source URL.
6. Save the source.

The worker downloads and parses the source. The reconciliation job creates canonical channels after the refresh completes.

## Verify channels

1. Open the **Channels** page.
2. Confirm that channels appear in the list.
3. Use the group filter to narrow the list.
4. Use the search field to find a channel by name.

## Configure Jellyfin

1. Open the **Jellyfin setup** page.
2. Copy the M3U output URL.
3. Copy the XMLTV output URL.
4. Add both URLs to Jellyfin as an M3U tuner.
5. Use the output token from `IPTV_OUTPUT_TOKEN`.

Jellyfin reads the playlist and guide from the token-protected endpoints.

## Next steps

- [Docker Compose](configuration/docker-compose.md): Learn the service topology.
- [Environment Variables](configuration/environment-variables.md): See the full variable reference.
- [Sources](configuration/sources.md): Add more source types.
