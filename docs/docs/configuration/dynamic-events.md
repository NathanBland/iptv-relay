# Dynamic-Event Records

An event template is a stored regular-expression rule. The scan endpoint stores provider-stream matches as event-channel records.

The scan parses event dates, times, teams, and timezones with built-in sports rules.
The scan creates durable event and filler programmes for matched channels.
The scan publishes these programmes through the XMLTV output.
The scan does not create a canonical channel.

## Template fields

| Field | Stored content |
|---|---|
| `displayName` | The template display name. |
| `groupName` | The source group name. |
| `matchRegex` | The PostgreSQL regular expression. |
| `channelNameFormat` | The event title format. Named regex captures and `{source}` or `{title}` are supported. |
| `eventDurationHours` | Stored duration value. |
| `pastDateGraceHours` | Stored retention value. |
| `futureDateDays` | Stored search-window value. |
| `timezone` | IANA timezone for event timestamps without an offset. Defaults to `UTC`. |
| `fillerTitle` | Title for filler programmes around events. Defaults to `No programs available`. |

The scan uses the template regular expression to select provider streams. It then applies the built-in sports rules to extract event data from matching stream names.

The current scan can link a record to the first enabled channel in the configured group. The scan does not create a canonical channel.

## Example regular expression

Use this expression to match a possible NFL stream name.

```text
(?i)^NFL\s+(?P<home>.+?)\s+vs\.?\s+(?P<away>.+?)\s+(\d{4}-\d{2}-\d{2})
```

The scan stores the matched source title as programme provenance. It applies the template timezone, duration, title, and filler settings to generated programmes. The `channelNameFormat` field supplies the event title format. Named regex captures and `{source}` or `{title}` are supported. Numbered placeholders such as `{1}` or `{2}` are not supported.

## Create a template record

Use this endpoint to create an event-template record.

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
    "futureDateDays": 7
  }'
```

The `name`, `displayName`, `matchRegex`, `channelNameFormat`, and `groupName` fields are required. The duration, grace, future-window, timezone, and filler-title fields use defaults when you omit them.

Set `timezone` to a valid IANA timezone, such as `America/Denver`, when event dates omit an offset.
Set `fillerTitle` to the title that the guide must use for contiguous filler programmes.

## Suggest a template record

Use this endpoint to list suggested templates from live provider stream names.

```bash
curl http://localhost:8080/api/v1/event-templates/suggestions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of suggestion objects. Each object includes the suggested fields and a sample of matching stream names. Use a suggestion as a starting point. Adjust the regular expression before you create the template.

## Scan or prune records

Use this endpoint to store records for matching provider streams.

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/scan \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Use this endpoint to hide old event-channel records.

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/prune \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The prune endpoint uses stored event end values.

## List records

Use this endpoint to list event-channel records for one template.

```bash
curl "http://localhost:8080/api/v1/event-channels?templateId={template_id}" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## User interface

The `/events` page manages template and event-channel records.
Use **Configure templates** to create a template with timezone and filler-title settings.
Use **Edit** on an existing template to update those settings and the matching rule.
