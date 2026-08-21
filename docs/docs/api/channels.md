# Channels API

The channels API lists, creates, and manages canonical channels and groups.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/channels` | List channels with pagination and search. |
| POST | `/api/v1/channels` | Create a channel. |
| GET | `/api/v1/channels/{channel_id}/preview` | Return the preview stream URL. |
| GET | `/api/v1/channels/{channel_id}/stream` | Proxy the MPEG-TS stream. |
| PATCH | `/api/v1/channels/{channel_id}/enabled` | Enable or disable a channel. |
| GET | `/api/v1/channels/{channel_id}/best-stream` | Return the best available stream. |
| POST | `/api/v1/channels/{channel_id}/stream-profile` | Assign a stream profile. |
| DELETE | `/api/v1/channels/{channel_id}/stream-profile` | Remove a stream profile assignment. |
| PATCH | `/api/v1/channels/{channel_id}/epg-mapping` | Set a manual EPG mapping. |
| DELETE | `/api/v1/channels/{channel_id}/epg-mapping` | Remove an EPG mapping. |
| GET | `/api/v1/groups` | List groups with channel counts. |
| PATCH | `/api/v1/groups/{group_name}/enabled` | Enable or disable a group. |
| PATCH | `/api/v1/groups/enabled` | Enable or disable all groups. |

## List channels

```bash
curl "http://localhost:8080/api/v1/channels?limit=50&offset=0&search=news" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns `total`, `limit`, `offset`, and `items`. Each item includes an `enabled` field.

## Enable or disable a channel

```bash
curl -X PATCH http://localhost:8080/api/v1/channels/{channel_id}/enabled \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": false}'
```

## List groups

```bash
curl http://localhost:8080/api/v1/groups \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response includes `channelCount` and `enabledCount` for each group.

## Enable or disable a group

```bash
curl -X PATCH http://localhost:8080/api/v1/groups/{group_name}/enabled \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

## Enable or disable all groups

```bash
curl -X PATCH http://localhost:8080/api/v1/groups/enabled \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

## Stream preview

```bash
curl http://localhost:8080/api/v1/channels/{channel_id}/preview \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns the stream URL with a token query parameter and the content type. The endpoint returns `503` when the channel has no active provider stream.

## Best stream

```bash
curl http://localhost:8080/api/v1/channels/{channel_id}/best-stream \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns the stream ordered by health then quality.
