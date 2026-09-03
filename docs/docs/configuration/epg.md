# EPG

The gateway maps canonical channels to EPG channels from XMLTV sources. The reconciliation runs in four passes.

## Timezone handling

The gateway stores XMLTV programme start and stop instants in UTC.
Set the XMLTV source `timezone` when a timestamp has no numeric offset or `Z` marker.
The source timezone must be a valid IANA name, such as `America/Denver`.
An explicit offset or `Z` marker takes precedence over the source timezone.
The gateway retains the original timestamp values for review.

M3U entries provide channel metadata, not programme times.
Map an M3U `tvg-id` value to the matching XMLTV `channel` ID during reconciliation.
Keep these identifiers stable across source refreshes when the provider supports stable IDs.
Do not use the M3U source timezone to shift guide times.

If the guide shows only future programmes, verify the raw XMLTV timestamp format first.
Use the provider timezone only when the raw value has no offset.
Leave the setting as `UTC` when the provider omits the offset from UTC values.

## Automatic matching

| Pass | Match method | Confidence | Review status |
|------|--------------|------------|---------------|
| 1 | Exact case-insensitive `tvg-id` | 0.99 | `applied` |
| 2 | Normalized name | 0.90 | `applied` |
| 3 | Channel alias | 0.85 | `applied` |
| 4 | Ambiguous normalized name | _varies_ | `review` |

The `normalize_channel_name` SQL function strips country prefixes, quality tokens, resolution markers, and punctuation from channel names.

The name match uses only the latest active XMLTV snapshot. The name match applies only when exactly one EPG channel shares the normalized name. The name match excludes VOD content.

## Channel alias match

Pass 3 reconciles channels that pass 1 and pass 2 did not map. The pass joins the channel name to the `channel_aliases` table on the normalized alias value. When the alias canonical name and an EPG display name share the same normalized form, the pass creates a mapping.

The alias match uses confidence `0.85` and the `alias` method. The mapping receives the `applied` review status. The pass applies only when exactly one EPG channel shares the normalized canonical name. The pass skips channels that already have a mapping. The pass preserves manual mappings.

## Trigger reconciliation

Trigger reconciliation with the API:

```bash
curl -X POST http://localhost:8080/api/v1/epg/reconcile \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns mapping statistics.

## Review queue

Ambiguous matches populate the `review_candidates` table. The mapping receives the `review` status.

List review candidates for a channel:

```bash
curl http://localhost:8080/api/v1/epg/review/{channel_id}/candidates \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Accept or reject a review candidate:

```bash
curl -X POST http://localhost:8080/api/v1/epg/review/{channel_id}/resolve \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"accept": true, "epgChannelId": "..."}'
```

Set `accept` to `true` to accept a candidate. Set `accept` to `false` to reject a candidate. Supply `epgChannelId` when you accept a candidate.

## Manual overrides

Set a manual EPG mapping for a channel:

```bash
curl -X PATCH http://localhost:8080/api/v1/channels/{channel_id}/epg-mapping \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"epgChannelId": "..."}'
```

Manual mappings receive the `manual` review status. Reconciliation preserves manual mappings.

Search EPG channels for manual linking:

```bash
curl "http://localhost:8080/api/v1/epg/channels/search?q=bbc" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Remove a mapping:

```bash
curl -X DELETE http://localhost:8080/api/v1/channels/{channel_id}/epg-mapping \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## EPG mappings page

The `/epg-mappings` page provides three tabs:

- **Mapped**: List applied and manual mappings with confidence and method badges.
- **Needs review**: List mappings in the `review` status with accept and reject actions.
- **Unmapped**: List channels with no EPG mapping and link them manually.

Select **Reconcile now** to trigger reconciliation from the UI.

## Enabled-channel filter

The `list_programmes`, `list_epg_mappings`, and `list_unmapped_channels` queries filter to channels where `enabled = true`. Disable a channel to hide it from the guide.

The programmes query orders the current programme before upcoming programmes.
The TV Guide page checks each programme against the browser clock.
The page formats programme times in the browser timezone.
