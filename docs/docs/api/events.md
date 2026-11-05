# Events API

The events API manages dynamic event templates and event-channel records. A scan matches active provider stream names with the built-in sports rules and stores generated event and filler programmes.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/events` | List dynamic events. |
| GET | `/api/v1/event-templates` | List event templates. |
| POST | `/api/v1/event-templates` | Create an event template. |
| PATCH | `/api/v1/event-templates/{template_id}` | Update an event template. |
| DELETE | `/api/v1/event-templates/{template_id}` | Delete an event template. |
| GET | `/api/v1/event-templates/suggestions` | List suggested event templates from live stream data. |
| GET | `/api/v1/event-channels` | List event channels. |
| POST | `/api/v1/event-templates/{template_id}/scan` | Scan provider streams for event channels. |
| POST | `/api/v1/event-templates/{template_id}/prune` | Hide ended event channels of one template. |

## List event templates

```bash
curl http://localhost:8080/api/v1/event-templates \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Create an event template

```bash
curl -X POST http://localhost:8080/api/v1/event-templates \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "nfl",
    "displayName": "NFL",
    "groupName": "Sports",
    "matchRegex": "(?i)^NFL\\s+.+?\\s+vs\\.?\\s+.+?\\s+\\d{4}-\\d{2}-\\d{2}",
    "channelNameFormat": "NFL: {source}",
    "eventDurationHours": 4,
    "pastDateGraceHours": 6,
    "futureDateDays": 7,
    "timezone": "America/Denver",
    "fillerTitle": "No programs available"
  }'
```

The `name`, `displayName`, `matchRegex`, `channelNameFormat`, and `groupName` fields are required. The duration, grace, future-window, timezone, and filler-title fields use defaults when you omit them.

The `channelNameFormat` field supplies the event title format. `{source}` and `{title}` render the provider stream name. Named captures from the built-in sports rules are supported when a built-in rule matches. The `matchRegex` value is a PostgreSQL advanced regular expression and does not support Python or Rust named-capture syntax. Numbered placeholders such as `{1}` or `{2}` are not supported.

## Update an event template

Send a partial update. Omitted fields keep their stored values. Set `enabled` to toggle the template.

```bash
curl -X PATCH http://localhost:8080/api/v1/event-templates/{template_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabled": false, "eventDurationHours": 5, "timezone": "America/Denver", "fillerTitle": "Off air"}'
```

Set `timezone` to a valid IANA timezone for event dates without an offset. Set `fillerTitle` to the title for generated filler programmes. The server rejects blank or invalid values.

## Delete an event template

```bash
curl -X DELETE http://localhost:8080/api/v1/event-templates/{template_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## List event-template suggestions

List suggested event templates that the gateway derives from live provider stream names:

```bash
curl http://localhost:8080/api/v1/event-templates/suggestions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of `EventTemplateSuggestionResponse` objects. Each object includes the suggested `name`, `displayName`, `matchRegex`, `channelNameFormat`, `groupName`, duration fields, `sampleStreams`, and `streamCount`.

Use a suggestion as a starting point. Adjust the regular expression before you create the template.

## List events

```bash
curl http://localhost:8080/api/v1/events \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response is an array of event objects. Each object has these keys:

- `id`: the event channel ID.
- `group` and `template`: the name of the template.
- `rawTitle`: the provider stream name.
- `programmeTitle`: the event title.
- `channelSlot`: the slot number as text.
- `channelId`: the ID of the target channel, or `null`.
- `outputName`: the output name of the target channel, or `null`.
- `start` and `end`: the event times in UTC, or `null` when the event has no time.
- `state`: `scheduled`, `live`, `ended`, or `hidden`.

## List event channels

```bash
curl "http://localhost:8080/api/v1/event-channels?templateId={template_id}" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Each object has the keys `id`, `templateId`, `channelId`, `slotNumber`, `eventTitle`, `eventStart`, `eventEnd`, `rawStreamName`, `outputName`, and `state`. The `outputName` key is the output name of the target channel. The event times are in UTC.

The `state` value is one of these:

- `scheduled`: the event starts in the future.
- `live`: the event is in progress.
- `ended`: the event stopped, and the grace period of the template did not end.
- `hidden`: the prune step hid the event channel.

## Scan event channels

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/scan \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The scan examines the enabled channels of active snapshots. It returns a scan report:

```json
{
  "ok": true,
  "message": "Scanned 9 channels: 2 matched, 1 scheduled, 1 skipped",
  "channelsScanned": 9,
  "matched": 2,
  "scheduled": 1,
  "skipped": 1,
  "pruned": 0,
  "skips": [
    {
      "templateId": "...",
      "channelId": "...",
      "streamName": "NHL 02 Rangers maybe later",
      "reason": "..."
    }
  ]
}
```

These keys give the counts:

- `channelsScanned`: the number of channels that the scan examined.
- `matched`: the number of channels that the template and a sports rule matched.
- `scheduled`: the number of channels that received event programmes.
- `skipped`: the number of matched channels that received no programmes.
- `pruned`: the number of event channels that the scan hid. A scan of one template does not prune, so the value is 0.

The `skips` array lists the reason for each skipped channel. The array holds at most 200 records. A skip record with no `channelId` is a reason for the template. An example is a `matchRegex` value that is not a valid Rust regular expression. When a template has zero matches, read the `skips` array to find the cause. The `message` key is a text summary of the counts.

The scan does not create a canonical channel. It ignores provider streams without a supported event date and time. The server returns 404 when the template does not exist. A disabled template returns a report with all counts at 0.

## Prune ended event channels

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/prune \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The prune step changes the state of an ended event channel to `hidden`. It uses the `pastDateGraceHours` value of the template in the path. The response is `{"ok": true, "message": "Pruned N event channels"}`. The server returns 404 when the template does not exist.

The worker runs a scan and a prune step for all templates after each provider reconciliation. Each template uses its own grace period.
