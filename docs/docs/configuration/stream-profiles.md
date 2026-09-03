# Stream-Profile Records

A stream profile is a stored configuration record. The current media path does not read or execute a stream profile.

Create and assign profiles only when you need to store future media settings. A profile assignment does not change the stream URL, adapter, command, buffer, user agent, or referer.

The v1 media plan supports native HTTP, FFmpeg, and VLC adapters. The current profile schema also accepts `streamlink` and `custom` values.

Do not use `streamlink` or `custom` values for v1 deployments. Do not rely on a profile assignment to change playback behavior.

## Stored type values

| Type | Stored value |
|---|---|
| `direct` | A profile type value. |
| `ffmpeg` | A profile type value. |
| `vlc` | A profile type value. |
| `streamlink` | A profile type value. |
| `custom` | A profile type value. |

Migration `0017_stream_profiles.sql` adds default records for Direct, FFmpeg, VLC, and Streamlink. The migration also adds `channel_stream_profiles` assignment records.

## Stored fields

The JSON request and response bodies use snake_case field names. The OpenAPI schema for `StreamProfileResponse` and `CreateStreamProfileRequest` defines these fields.

| Field | Stored content |
|---|---|
| `name` | The profile name. |
| `profile_type` | A profile type value. |
| `command` | A command text value. |
| `arguments` | An argument array value. |
| `buffer_seconds` | A buffer-duration value. |
| `user_agent` | A user-agent value. |
| `referer` | A referer value. |

The current media path ignores every field in this table. Do not store provider credentials in a profile record.

## Create a profile record

Use this endpoint to create a stream-profile record.

```bash
curl -X POST http://localhost:8080/api/v1/stream-profiles \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "FFmpeg HD",
    "profile_type": "ffmpeg",
    "buffer_seconds": 5,
    "user_agent": "VLC/3.0"
  }'
```

Use this endpoint to list stream-profile records.

```bash
curl http://localhost:8080/api/v1/stream-profiles \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of `StreamProfileResponse` objects. Each object uses the snake_case field names from the OpenAPI schema:

```json
[
  {
    "id": "11111111-1111-1111-1111-111111111111",
    "name": "FFmpeg HD",
    "profile_type": "ffmpeg",
    "command": null,
    "arguments": [],
    "buffer_seconds": 5,
    "user_agent": "VLC/3.0",
    "referer": null,
    "enabled": true,
    "created_at": "2025-01-01T00:00:00Z",
    "updated_at": "2025-01-01T00:00:00Z"
  }
]
```

Use this endpoint to delete a stream-profile record.

```bash
curl -X DELETE http://localhost:8080/api/v1/stream-profiles/{profile_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Assign a profile record

Use this endpoint to store a profile assignment for one channel.

```bash
curl -X POST http://localhost:8080/api/v1/channels/{channel_id}/stream-profile \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"stream_profile_id": "..."}'
```

Use this endpoint to remove all profile assignments from one channel.

```bash
curl -X DELETE http://localhost:8080/api/v1/channels/{channel_id}/stream-profile \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## User interface

The `/stream-profiles` page manages profile records. The page does not configure the media process.
