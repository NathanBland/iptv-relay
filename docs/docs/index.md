# IPTV Gateway

IPTV Gateway is an MIT-licensed Rust appliance. It turns M3U and Xtream live-TV sources and XMLTV guide data into stable, token-protected endpoints for Jellyfin.

## Legal notice

You must use only streams that you have the legal right to access.
This project does not provide streams, channels, provider accounts, playlists, programme data, or media content.
You are responsible for the providers, URLs, credentials, and content that you configure.
The maintainers do not control or endorse third-party content.
The maintainers are not responsible for the use of third-party content.

## Appliance install

The pull-only appliance bundle uses the signed release images from GitHub Container Registry.
You do not clone the repository and you do not compile code in this path.

The bundle publishes three application images: `core` and `worker`, `web`, and the `gateway` reverse proxy.
The Caddy configuration is baked into the gateway image, so you do not need `deploy/Caddyfile` on the host.
The appliance Compose file starts the full stack: `gateway`, `web`, `core`, `worker`, and `postgres`.

See [Quick Start](quick-start.md) for the appliance install steps, platform guides, upgrades, rollbacks, and backups.

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

## Feature highlights

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
- TV guide page.

XMLTV times use UTC internally. Set the source timezone when XMLTV values omit an offset.
An explicit offset or `Z` marker takes precedence over the source timezone.
The M3U source does not provide programme times, so its timezone does not shift channel metadata.
The guide retains the original XMLTV value after it stores the normalized instant in UTC.
The TV Guide page shows the current programme first and uses the browser timezone for display.

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

## Next steps

- [Quick Start](quick-start.md): Start the appliance with the pull-only Compose bundle.
- [Configuration](configuration/docker-compose.md): Configure services and volumes.
- [API Reference](api/authentication.md): Learn the control API.
- [Development](development/local-setup.md): Set up a local development environment.
