# Dynamic Events

The event scan finds sports events in provider stream names. It writes event programmes and filler programmes to the guide of the channel that carries the stream.

The scan does not create channels. It writes guide data to existing enabled channels only.

## When the scan runs

The scan runs automatically after each provider refresh publishes the catalog. The refresh job then prunes ended events for each template.

A scan error does not stop the refresh job. The job writes a warning to the log and completes.

You can also start a scan for one template through the API. Refer to [Scan or prune records](#scan-or-prune-records).

## Built-in detection

The scan applies a built-in template to all enabled channels. You do not have to configure a template.

The built-in template has these properties:

- Name: `built-in-sports`.
- ID: `8b2d571e-91de-7000-8000-00000000e000`.
- `matchRegex`: `.*`.
- Event duration: 3 hours.
- Grace after the event: 4 hours.
- Future limit: 7 days.

The built-in sports rules read the league, the slot number, the teams, the date, the start time, and the stop time from the stream name. The rules accept these name formats:

| Example stream name | Event start |
|---|---|
| `NHL: 01 - 7:30pm Senators @ Sabres [1080p]` | Time only. The scan uses the date of the provider snapshot. |
| `Detroit Pistons vs Philadelphia 76ers @ Nov 4 7:00 PM :NBA  01 [1080p]` | Month, day, and time. |
| `CA (TSN+ 007): NCAA Football: Ball State vs. Massachusetts (2026-11-04 19:00:00) [1080p]` | Full date and time. |
| `MLB 01:  Brewers x Rays start:2026-11-04 20:00:00 stop:2026-11-04 23:13:20 [1080p]` | Start and stop. |

To stop the built-in detection, disable the `built-in-sports` template.

## Operator templates

An operator template limits the scan to stream names that match its `matchRegex`. The template also sets the title format, the duration, the grace, and the timezone.

The scan applies operator templates before the built-in template. It applies operator templates in the order of their names. The first template with a matching stream owns the channel.

A stream name must also match a built-in sports rule. If the template regex matches but no sports rule matches, the scan records a skip.

## Template fields

| Field | Content |
|---|---|
| `displayName` | The display name of the template. |
| `groupName` | The source group name. |
| `matchRegex` | A Rust regular expression. Refer to [Regular expression dialect](#regular-expression-dialect). |
| `channelNameFormat` | The title format of the event programme. |
| `eventDurationHours` | The event duration when the stream name has no stop time. The minimum is 1 hour. |
| `pastDateGraceHours` | The time after the event end that the guide keeps the event. The minimum is 1 hour. |
| `futureDateDays` | The scan skips an event that starts later than this number of days. |
| `timezone` | The IANA timezone for event times without an offset. The default is `UTC`. |
| `fillerTitle` | The stored filler title. The event scan uses its own filler titles. |

## Regular expression dialect

The `matchRegex` value uses the syntax of the Rust `regex` crate.

Use `\b` for a word boundary. Use `(?P<name>...)` or `(?<name>...)` for a named group.

The scan also accepts the PostgreSQL word-boundary escapes `\y`, `\m`, and `\M`. The scan changes each escape to `\b`.

Migration `0077` changes these escapes to `\b` in stored templates.

In PostgreSQL syntax, `\b` is a backspace character. Do not use PostgreSQL syntax for new templates.

Use this expression to match NFL stream names:

```text
(?i)^NFL\b.*\b(?:vs\.?|at)\b
```

## Timezone order

A stream name can give a time without an offset. The scan selects the timezone in this order:

1. The template timezone, if it is not `UTC`.
2. The source timezone of the provider account, if it is not `UTC`.
3. The timezone in the region settings.
4. `UTC`.

## Title format

The `channelNameFormat` field gives the title of the event programme. The built-in template uses `{event}`.

These placeholders are available:

- `{event}`: the two teams, for example `Senators @ Sabres`.
- `{home}` and `{away}`: the teams, when the rule finds them.
- `{league}`: the league, when the rule finds it.
- `{source}` and `{title}`: the provider stream name.

The scan does not support numbered placeholders such as `{1}`.

## Channel names

The scan gives a stable output name to each channel with a parsed event. The name does not change when the event changes.

The scan makes the name from the league and the slot number, for example `NHL 01`. If the stream name has no slot, the scan uses the provider label, for example `TSN+ 007`.

The output name has this precedence:

1. The name override of the channel.
2. A channel alias.
3. The stable event name.
4. The provider name.

A slot without an event, for example `NFL : 01 - [1080p]`, keeps the provider name.

## Guide programmes

The scan writes these programmes on each event channel:

1. An upcoming programme from the start of the current hour to the event start. The title has this format: `Upcoming: Senators @ Sabres, Nov 4 7:30 PM EST`.
2. The event programme.
3. An `Event ended` programme from the event end to the end of the grace.

The programmes are contiguous. The scan writes no programmes to a slot without an event.

## Event state

The scan sets the state of each event channel from the current time:

| State | Condition |
|---|---|
| `scheduled` | The event has not started. |
| `live` | The event started and has not ended. |
| `ended` | The event ended, and the grace has not ended. |
| `hidden` | The grace ended, or no stream matches the template. |

The guide output includes the `scheduled`, `live`, and `ended` states.

## Guide coverage

The guide coverage value of `/api/v1/system` counts a channel with an EPG mapping. It also counts a channel with event programmes.

## Create a template record

Use this endpoint to create an event template.

```bash
curl -X POST http://localhost:8080/api/v1/event-templates \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "nfl",
    "displayName": "NFL",
    "groupName": "Sports",
    "matchRegex": "(?i)^NFL\\b.*\\b(?:vs\\.?|at)\\b",
    "channelNameFormat": "{away} at {home}",
    "eventDurationHours": 4,
    "pastDateGraceHours": 6,
    "futureDateDays": 7
  }'
```

The `name`, `displayName`, `matchRegex`, `channelNameFormat`, and `groupName` fields are necessary. If you omit the other fields, the server uses default values.

## Suggest a template record

Use this endpoint to get suggested templates from provider stream names.

```bash
curl http://localhost:8080/api/v1/event-templates/suggestions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response is an array of suggestions. Each suggestion has the template fields and some matching stream names. Examine the regular expression before you create the template.

## Scan or prune records

Use this endpoint to scan the streams for one template.

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/scan \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response gives the number of channels that received event programmes.

Use this endpoint to hide old event channels.

```bash
curl -X POST http://localhost:8080/api/v1/event-templates/{template_id}/prune \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## List records

Use this endpoint to list the event channels of one template.

```bash
curl "http://localhost:8080/api/v1/event-channels?templateId={template_id}" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## User interface

The `/events` page shows templates and event channels.

- To create a template, use **Configure templates**.
- To change a template, use **Edit**.
