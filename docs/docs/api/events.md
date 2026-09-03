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
| POST | `/api/v1/event-templates/{template_id}/prune` | Prune past event channels. |

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
    "matchRegex": "(?i)^NFL\\s+(?P<home>.+?)\\s+vs\\.?\\s+(?P<away>.+?)\\s+(\\d{4}-\\d{2}-\\d{2})",
    "channelNameFormat": "NFL: {home} vs {away}",
    "eventDurationHours": 4,
    "pastDateGraceHours": 6,
    "futureDateDays": 7,
    "timezone": "America/Denver",
    "fillerTitle": "No programs available"
  }'
```

The `name`, `displayName`, `matchRegex`, `channelNameFormat`, and `groupName` fields are required. The duration, grace, future-window, timezone, and filler-title fields use defaults when you omit them.

The `channelNameFormat` field supplies the event title format. Named regex captures and `{source}` or `{title}` are supported. Numbered placeholders such as `{1}` or `{2}` are not supported.

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

## List event channels

```bash
curl "http://localhost:8080/api/v1/event-channels?templateId={template_id}" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Scan event channels

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/scan \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns the count of updated event channels. The scan does not create a canonical channel. It ignores provider streams without a supported event date and time.

## Prune past event channels

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/prune \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```
