# Sources API

The sources API manages M3U, Xtream, and XMLTV sources. All endpoints require admin authentication.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/sources` | List configured sources. |
| POST | `/api/v1/sources` | Create a source. |
| PATCH | `/api/v1/sources/{source_id}` | Update a source. |
| DELETE | `/api/v1/sources/{source_id}` | Delete a source and related data. |
| PATCH | `/api/v1/sources/{source_id}/refresh-interval` | Set the refresh interval. |
| POST | `/api/v1/sources/{source_id}/sync` | Trigger a manual source sync. |

## List sources

```bash
curl http://localhost:8080/api/v1/sources \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response includes `refreshIntervalSeconds`, `lastRefreshedAt`, `maxConnections`, `timezone`, and `enabled` for each source.

The optional `timezone` value defaults to `UTC`.
Use a valid IANA timezone name for XMLTV timestamps without an explicit offset.
The source timezone affects XMLTV timestamps only when the timestamp omits an offset.
An explicit numeric offset or `Z` marker takes precedence over the source timezone.
M3U records do not contain programme times, so the source timezone does not shift M3U channel metadata.

## Create a source

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

Set `timezone` in the request when the XMLTV source uses local wall-clock values without offsets:

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

Leave `timezone` unset when the source uses UTC values without offsets.

## Update a source

```bash
curl -X PATCH http://localhost:8080/api/v1/sources/{source_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"maxConnections": 8, "timezone": "America/Denver", "enabled": true}'
```

The endpoint rejects `maxConnections` values less than 1.

## Set the refresh interval

```bash
curl -X PATCH http://localhost:8080/api/v1/sources/{source_id}/refresh-interval \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"refreshIntervalSeconds": 3600}'
```

A value of `0` disables automatic refresh.

## Trigger a manual sync

```bash
curl -X POST http://localhost:8080/api/v1/sources/{source_id}/sync \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns `202` with the job ID. A second sync request returns `409` when a refresh job is already active.

## Delete a source

```bash
curl -X DELETE http://localhost:8080/api/v1/sources/{source_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The deletion removes the source record and all related data.
