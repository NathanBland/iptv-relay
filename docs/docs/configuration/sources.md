# Sources

A source is an M3U playlist, an Xtream Codes account, or an XMLTV guide. The worker downloads, parses, and activates each source on a schedule.

## Source types

| Type | Description |
|------|-------------|
| M3U | HTTP playlist of `#EXTINF` stream entries. |
| Xtream | Xtream Codes live-stream account. |
| XMLTV | XMLTV guide document. |

## Add a source with the API

Create an M3U source:

```bash
curl -X POST http://localhost:8080/api/v1/sources \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "m3u",
    "name": "My M3U source",
    "url": "https://example.com/playlist.m3u"
  }'
```

The endpoint returns the created source with its ID. The worker enqueues a `refresh-source` job.

### Set the XMLTV timezone

Set `timezone` when an XMLTV source contains local timestamps without offsets.
Use a valid IANA timezone name.
The default value is `UTC`.
An explicit numeric offset or `Z` marker takes precedence over this setting.
M3U records do not contain programme times, so this setting does not shift M3U metadata.

```bash
curl -X POST http://localhost:8080/api/v1/sources \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "xmltv",
    "name": "Local XMLTV guide",
    "url": "https://example.com/guide.xml",
    "timezone": "America/Denver"
  }'
```

## Add a source with the UI

1. Open the **Sources** page.
2. Select **Add source**.
3. Choose the source type.
4. Enter the source name and URL.
5. Save the source.

The sources page shows the source state, last refreshed time, and refresh interval.

## Refresh intervals

Each source has a `refreshIntervalSeconds` value. A value of `0` disables automatic refresh.

Set the refresh interval with the API:

```bash
curl -X PATCH http://localhost:8080/api/v1/sources/{source_id}/refresh-interval \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"refreshIntervalSeconds": 3600}'
```

The refresh scheduler runs every 60 seconds. It enqueues a `refresh-source` job for each due source.

## Manual sync

Trigger a manual sync for one source:

```bash
curl -X POST http://localhost:8080/api/v1/sources/{source_id}/sync \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns `202` with the job ID. A second sync request returns `409` when a refresh job is already active.

## Edit a source

Update the max connections, timezone, or enabled flag:

```bash
curl -X PATCH http://localhost:8080/api/v1/sources/{source_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"maxConnections": 8, "timezone": "America/Denver", "enabled": true}'
```

The endpoint rejects `maxConnections` values less than 1.

The worker stores normalized programme instants in UTC.
The worker retains the original XMLTV start and stop values for source review.

## Delete a source

Delete a source and its related data:

```bash
curl -X DELETE http://localhost:8080/api/v1/sources/{source_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The deletion removes the source record, provider streams, and related catalog data.

## Reconciliation

The worker calls reconciliation after each successful source refresh. Reconciliation groups provider streams by `tvg-id` and merges streams that share the same identifier. Streams without a `tvg-id` receive a fallback canonical key.
