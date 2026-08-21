# Channels and Groups

Channels are canonical catalog entries. Each channel has a stable local UUID. Groups collect channels by their provider group name.

## List channels

The channels API returns a paginated response with `total`, `limit`, `offset`, and `items`.

```bash
curl "http://localhost:8080/api/v1/channels?limit=50&offset=0&search=news" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The `search` parameter filters channels by name with a case-insensitive match.

## Enable and disable channels

Enable or disable one channel:

```bash
curl -X PATCH http://localhost:8080/api/v1/channels/{channel_id}/enabled \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": false}'
```

The playlist and XMLTV endpoints include selected channels where `enabled = true`.

## Groups

List groups with channel counts:

```bash
curl http://localhost:8080/api/v1/groups \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response includes `channelCount` and `enabledCount` for each group.

Enable or disable all channels in one group:

```bash
curl -X PATCH http://localhost:8080/api/v1/groups/{group_name}/enabled \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

Enable or disable all groups at once:

```bash
curl -X PATCH http://localhost:8080/api/v1/groups/enabled \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

## Channels page

The channels page provides:

- A searchable group filter combobox.
- Server-side search with debounce.
- An enable or disable toggle per channel.
- Enable-all and disable-all buttons when a group filter is active.
- A stream preview button per channel.

## Groups page

The `/groups` page provides:

- A search input to filter groups by name.
- Bulk enable and disable buttons for all groups.
- A summary count of enabled and total channels.
- A card per group with a status badge.
- Enable-all and disable-all buttons per group.

## Stream preview

The preview button opens a video element that plays the raw MPEG-TS stream with `mpegts.js`.

The preview endpoint returns the stream URL with a token query parameter. The stream endpoint proxies the MPEG-TS stream through the admin session.
