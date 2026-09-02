# EPG API

The EPG API lists programmes and manages EPG mappings between canonical channels and XMLTV EPG channels.

The API stores programme instants in UTC.
The API retains source timestamp values for review.
The TV Guide page displays the current programme before upcoming programmes.
The browser formats displayed times in its local timezone.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/programmes` | List programmes with pagination and search. |
| POST | `/api/v1/epg/reconcile` | Trigger EPG reconciliation. |
| GET | `/api/v1/epg/mappings` | List EPG mappings. |
| GET | `/api/v1/epg/unmapped` | List channels with no EPG mapping. |
| GET | `/api/v1/epg/review/{channel_id}/candidates` | List review candidates. |
| GET | `/api/v1/epg/channels/search` | Search EPG channels. |
| PATCH | `/api/v1/channels/{channel_id}/epg-mapping` | Set a manual EPG mapping. |
| DELETE | `/api/v1/channels/{channel_id}/epg-mapping` | Remove an EPG mapping. |
| POST | `/api/v1/epg/review/{channel_id}/resolve` | Accept or reject a review candidate. |

## List programmes

```bash
curl "http://localhost:8080/api/v1/programmes?limit=50&offset=0&search=france" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The `search` parameter filters programmes by title or channel name with a case-insensitive match. The response returns `total`, `limit`, `offset`, and `items`.

The response uses ISO 8601 UTC timestamps for `start` and `end`.
The current programme has a start at or before the current time and an end after the current time.

## Trigger reconciliation

```bash
curl -X POST http://localhost:8080/api/v1/epg/reconcile \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns mapping statistics.

## List mappings

```bash
curl "http://localhost:8080/api/v1/epg/mappings?reviewStatus=review" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response includes confidence, method, and review status per mapping.

## List unmapped channels

```bash
curl "http://localhost:8080/api/v1/epg/unmapped?search=news" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## List review candidates

```bash
curl http://localhost:8080/api/v1/epg/review/{channel_id}/candidates \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Resolve a review

```bash
curl -X POST http://localhost:8080/api/v1/epg/review/{channel_id}/resolve \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"accept": true, "epgChannelId": "..."}'
```

Set `accept` to `true` to accept a candidate. Set `accept` to `false` to reject a candidate. Supply `epgChannelId` when you accept a candidate.

## Search EPG channels

```bash
curl "http://localhost:8080/api/v1/epg/channels/search?q=bbc" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Set a manual mapping

```bash
curl -X PATCH http://localhost:8080/api/v1/channels/{channel_id}/epg-mapping \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"epgChannelId": "..."}'
```

## Remove a mapping

```bash
curl -X DELETE http://localhost:8080/api/v1/channels/{channel_id}/epg-mapping \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```
