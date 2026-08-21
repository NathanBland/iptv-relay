# Sessions API

The sessions API lists active media sessions and provides realtime Server-Sent Events streams.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/sessions` | List active media sessions. |
| GET | `/api/v1/session-events` | Subscribe to session SSE events. |
| GET | `/api/v1/catalog-events` | Subscribe to catalog SSE events. |
| GET | `/api/v1/jobs` | List jobs. |
| POST | `/api/v1/jobs/{job_id}/cancel` | Cancel a job. |

## List sessions

```bash
curl http://localhost:8080/api/v1/sessions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response lists active media sessions with viewer counts and upstream details.

## Catalog SSE events

The `/api/v1/catalog-events` endpoint publishes `overview` events with system counts every five seconds. The stream sends keepalive comments between events.

The Caddy gateway disables buffering for SSE endpoints with `flush_interval -1`.

Connect to the SSE stream:

```bash
curl -N -b cookies.txt http://localhost:8080/api/v1/catalog-events
```

The frontend `useCatalogEvents` hook subscribes to the SSE stream from authenticated sessions. The hook invalidates the `overview` and `sources` React Query caches when events arrive.

## Session SSE events

The `/api/v1/session-events` endpoint publishes session lifecycle events. Use this stream to track viewer connections and upstream sessions in realtime.

## Jobs

List jobs:

```bash
curl http://localhost:8080/api/v1/jobs \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Cancel a job:

```bash
curl -X POST http://localhost:8080/api/v1/jobs/{job_id}/cancel \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```
