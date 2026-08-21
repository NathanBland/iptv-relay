# Stream-Profile Records

A stream profile is a stored configuration record. The current media path does not read or execute a stream profile.

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

| Field | Stored content |
|---|---|
| `name` | The profile name. |
| `profileType` | A profile type value. |
| `command` | A command text value. |
| `arguments` | An argument array value. |
| `bufferSeconds` | A buffer-duration value. |
| `userAgent` | A user-agent value. |
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
    "profileType": "ffmpeg",
    "bufferSeconds": 5,
    "userAgent": "VLC/3.0"
  }'
```

Use this endpoint to list stream-profile records.

```bash
curl http://localhost:8080/api/v1/stream-profiles \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
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
  -d '{"profileId": "..."}'
```

Use this endpoint to remove all profile assignments from one channel.

```bash
curl -X DELETE http://localhost:8080/api/v1/channels/{channel_id}/stream-profile \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## User interface

The `/stream-profiles` page manages profile records. The page does not configure the media process.
