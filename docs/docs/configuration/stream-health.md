# Stream Health

Stream health is a data model and an API surface. The current service does not run `ffprobe` or another stream probe.

The health-check endpoint marks selected streams as `checking`. No worker completes those checks or writes media metadata.

## Status values

| Status | Stored meaning |
|---|---|
| `alive` | A future probe can set this value. |
| `dead` | A future probe can set this value. |
| `unknown` | The stream has no stored result. |
| `checking` | The check endpoint selected the stream. |

## Stored fields

Migration `0013_stream_health_quality.sql` adds health and media metadata fields to `provider_streams`.

- Health fields: `health_status`, `health_checked_at`, and `health_error`.
- Video fields: codec, resolution, width, height, and frame rate.
- Audio fields: codec, channel count, and sample rate.
- Bitrate field: `bitrate_kbps`.

The migration also creates the `stream_health_checks` audit table. The current worker does not create audit rows.

## Mark streams for a future check

Use this endpoint to mark up to 50 eligible streams as `checking`.

```bash
curl -X POST http://localhost:8080/api/v1/streams/health/check \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint does not start a media probe. Do not use this endpoint as a stream-health test.

## List stored health data

Use this endpoint to list stored stream health fields.

```bash
curl "http://localhost:8080/api/v1/streams/health?status=dead" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Use this endpoint to get counts by stored status.

```bash
curl http://localhost:8080/api/v1/streams/health/stats \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Stored quality ranks

Use this endpoint to calculate stored quality ranks from existing metadata.

```bash
curl -X POST http://localhost:8080/api/v1/streams/rank \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The service stores the result in `channel_streams.quality_rank`. The playback route uses this rank after health status.

Use this endpoint to get the stored best-stream result for one channel.

```bash
curl http://localhost:8080/api/v1/channels/{channel_id}/best-stream \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The result shows the stored first choice. Playback opens ordered alternate streams for coordinated failover.

## User interface

The `/stream-health` page displays stored status and metadata. The page can mark streams as `checking` and calculate stored ranks.
