# Recording Metadata

The v1 plan excludes DVR media recording. Migration `0016_dvr_recordings.sql` adds recording-rule and recording metadata tables.

The current service has no recording scheduler, media writer, file manager, retention worker, or playback library. Do not use these endpoints to record media.

## Stored rule types

| Type | Stored value |
|---|---|
| `one-time` | A rule type value. |
| `recurring` | A rule type value. |
| `series` | A rule type value. |

## Stored recording states

| State | Stored value |
|---|---|
| `scheduled` | A recording status value. |
| `recording` | A recording status value. |
| `completed` | A recording status value. |
| `failed` | A recording status value. |
| `cancelled` | A recording status value. |

No runtime component changes these states. The service does not create media files.

## Create a rule record

Use this endpoint to create a recording-rule record.

```bash
curl -X POST http://localhost:8080/api/v1/recordings/rules \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Evening news",
    "channelId": "...",
    "ruleType": "one-time",
    "startPaddingMinutes": 2,
    "endPaddingMinutes": 5,
    "keepUntil": "one-week"
  }'
```

Use this endpoint to list recording-rule records.

```bash
curl http://localhost:8080/api/v1/recordings/rules \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Use this endpoint to delete a recording-rule record.

```bash
curl -X DELETE http://localhost:8080/api/v1/recordings/rules/{rule_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Recording records

Use this endpoint to list recording metadata.

```bash
curl "http://localhost:8080/api/v1/recordings?status=completed" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Use this endpoint to create a recording metadata record.

```bash
curl -X POST http://localhost:8080/api/v1/recordings \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"ruleId": "...", "channelId": "..."}'
```

Use this endpoint to delete a recording metadata record.

```bash
curl -X DELETE http://localhost:8080/api/v1/recordings/{recording_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Use this endpoint to get stored recording counts.

```bash
curl http://localhost:8080/api/v1/recordings/stats \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response reports database values. The response does not report disk usage from a recorder.

## User interface

The `/recordings` page manages metadata records. The page does not schedule or record media.
