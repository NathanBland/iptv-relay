# iptv-relay audit and fix plan

- Audit date: 2026-11-04.
- Audited version: v0.2.18 at `https://iptv-relay.aqueous.network/`.
- Branch: `claude/audit-plan-aa950e`, at base commit `90f39da`.
- Scope: this document is a plan only. This branch does not change code.

## 1. Method

The audit used these sources:

- The code in `crates/*`, `apps/server`, `apps/web`, and `migrations`.
- The documents in `docs/docs`.
- `GET` requests to the live instance with an admin bearer token. The audit sent no write requests and opened no streams.
- One public `GET` request to the ESPN scoreboard API.

All 33 `GET /api/v1/*` endpoints that the audit called returned HTTP 200. Thus the defects are contract defects and logic defects, not route failures.

Kaneo tracking: `AGENTS.md` requires the `iptv-relay` Kaneo project. The connected Kaneo instance does not contain this project, and the operator confirmed that it is not available now. Thus this audit did not create or update Kaneo tasks. Create one Kaneo task for each work package when the project is available again.

Redaction: this document replaces the output token in all URLs with `<token>`. It does not contain credentials.

## 2. Summary

| ID | Finding | Severity | Package |
|----|---------|----------|---------|
| F1 | Five API resource groups use snake_case JSON. The web UI sends and reads camelCase. | High | WP1 |
| F2 | Channel aliases never match the live channel names. | High | WP3 |
| F3 | No per-channel rename feature exists. | High | WP3, WP5 |
| F4 | The event scan has six separate blockers. It always produces zero event channels. | High | WP2, WP4 |
| F5 | No process runs the event scan automatically. Prune uses one template's grace for all templates. | Medium | WP4, WP7 |
| F6 | `GET /api/v1/events` returns a shape that the web client does not expect. | Low | WP7 |
| F7 | No sports metadata enrichment exists. | Feature | WP8a, WP8b |
| F8 | XMLTV output does not mark sports programmes. The omission of `sub-title` is possibly intentional. | Medium | WP8b |
| F9 | The runtime does not read six of the nine operator settings. v0.2.19 fixed the other three. | Medium | WP6 |
| F10 | Short-EPG refresh jobs fail. Guide coverage is 5.5 %. | Medium | WP10 |
| F11 | `/api/v1/system` reports `providerLimit: 0` when no session is active. | Low | WP6 |
| F12 | The Xtream source timezone is `UTC`, but the provider publishes US Eastern times. | Medium | Operator action, WP4 |
| F13 | Jellyfin shows 1080p and 20 Mbps for all relay channels, because the HDHomeRun lineup has no quality fields. | Medium | WP11 |
| F14 | The provider sends live data in bursts with gaps of up to 27 s. The relay forwards the bursts without pacing, so transcoding clients run dry. | High | WP12 |
| F15 | A stream that cannot start (pool full, provider error) gives an untested slate or an HTTP error. Jellyfin can fail to open the channel. | Medium | WP13 |
| F16 | 45 enabled channels carry the event, or an empty slot, in the channel name. The served guide has no programme for any of them, and their names change each day. | High | WP2b, WP4 |

## 3. Findings

### F1. API JSON casing does not agree with the web UI

Symptom: these tabs show empty fields, `NaN`, or "Invalid request" errors:

- **Channel aliases**
- **Stream health**
- **Stream profiles**
- **Recordings**
- **Users**

Evidence from the live instance (key names only):

```text
GET /api/v1/channel-aliases     item keys: alias, canonical_name, category, country, created_at, id
GET /api/v1/stream-profiles     item keys: arguments, buffer_seconds, command, created_at, enabled, id,
                                           name, profile_type, referer, updated_at, user_agent
GET /api/v1/streams/health      item keys: audio_channels, ..., health_status, stream_name, video_width, ...
GET /api/v1/recordings/stats    keys:      completed, failed, recording, scheduled, total_bytes
GET /api/v1/channels            item keys: bitrateKbps, enabled, group, id, name, number, primaryCodec, ...
```

A key comparison between `apps/web/src/lib/api/types.ts` and the live responses found these mismatches:

- `ChannelAlias`: the UI reads `canonicalName` and `createdAt`. The API sends `canonical_name` and `created_at`.
- `StreamHealthItem`: all 16 fields are snake_case in the API.
- `StreamProfile`: `profileType`, `bufferSeconds`, `userAgent`, `createdAt`, and `updatedAt` are snake_case in the API.
- `RecordingStats`: the UI reads `totalBytes`. The API sends `total_bytes`. Thus `formatBytes(undefined)` shows `NaN undefined` (`apps/web/src/pages/recordings-page.tsx:22`, `:127`).

Root cause: these structs in `crates/api/src/lib.rs` have no `#[serde(rename_all = "camelCase")]`. All other API structs have it.

| Struct | Line | Direction |
|--------|------|-----------|
| `StreamHealthItem` | 7445 | Response |
| `BestStreamResponse` | 7514 | Response |
| `UserResponse` | 7750 | Response |
| `CreateUserRequest` | 7777 | Request |
| `UpdateUserRequest` | 7785 | Request |
| `ChannelAliasResponse` | 7993 | Response |
| `CreateChannelAliasRequest` | 8022 | Request |
| `ResolveAliasResponse` | 8030 | Response |
| `RecordingRuleResponse` | 8391 | Response |
| `CreateRecordingRuleRequest` | 8428 | Request |
| `RecordingResponse` | 8441 | Response |
| `CreateRecordingRequest` | 8482 | Request |
| `RecordingStatsResponse` | 8499 | Response |
| `StreamProfileResponse` | 8830 | Response |
| `CreateStreamProfileRequest` | 8863 | Request |
| `AssignStreamProfileRequest` | 8874 | Request |
| `DeadLetterResponse` | 509 | Response (no web consumer) |

The request structs cause the form errors. For example, the alias form sends `{"canonicalName": ...}` (`apps/web/src/pages/channel-aliases-page.tsx:26`). Axum rejects the body because `canonical_name` is missing. The same defect affects **Add user** (`displayName`), **Add recording rule** (`channelId`), and **Add stream profile** (`profileType`). One call has the opposite defect: `assignStreamProfile` sends `stream_profile_id` (`apps/web/src/lib/api/client.ts:993`).

The documentation agrees with the web UI. `docs/docs/configuration/channel-aliases.md` shows a `canonicalName` request body. Thus the API is the defective side.

Other tabs: the audit compared the `Overview`, `Source`, `Job`, `Channel`, `Group`, `Programme`, `EpgMapping`, `EventTemplate`, `EventTemplateSuggestion`, settings, token, log, region, and Jellyfin types. These types agree with the live responses.

Fix:

1. Add `#[serde(rename_all = "camelCase")]` to each struct in the table, except `DeadLetterResponse`.
2. Change `assignStreamProfile` in `client.ts` to send `streamProfileId`.
3. Update the Rust tests that send snake_case bodies, for example `crates/api/src/lib.rs:12498` and `:13530`.
4. Regenerate `tests/fixtures/openapi.json` with `make openapi-snapshot`.
5. Add a guard test. The test walks `ApiDoc::openapi()` component schemas and fails on a property name that contains `_`. Put the allowed exceptions in an explicit allow list, for example `DeadLetterResponse`.

Decision: leave `DeadLetterResponse` as snake_case in this package. `docs/development/live-job-diagnostics.md` and operator scripts can use those keys. Change it in a separate commit only after a check of those consumers.

### F2. Channel aliases never match live channel names

Trace from save to output:

1. Save: `POST /api/v1/channel-aliases` stores a row in `channel_aliases` (`crates/persistence/src/catalog.rs:5906` and nearby). With the web UI, this step fails because of F1.
2. Output: `/out/<token>/playlist.m3u`, `/out/<token>/xmltv.xml`, and `/out/<token>/hdhr/lineup.json` all call `list_enabled_channels_for_output_profile` (`crates/api/src/lib.rs:6719`, `:6792`, `:7381`).
3. That query selects `coalesce(channel_alias.canonical_name, c.name) AS name` (`crates/persistence/src/catalog.rs:2675`). The lateral join compares `normalize_channel_name(ca.alias)` with `normalize_channel_name(c.name)` (`:2686`).
4. `normalize_channel_name` strips only a two-letter prefix: `'^[A-Z]{2}\s*[:|]\s*'` (`migrations/0007_normalize_channel_name.sql:27`).

The override is lost at step 4. All enabled live channels use three-letter prefixes or other prefixes:

```text
USA: NHL NETWORK [1080p]   -> normalize_channel_name -> "usa nhl network"
alias "NHL Network HD"     -> normalize_channel_name -> "nhl network"
```

The two keys never agree. Thus the `coalesce` always falls back to `c.name`. The live playlist has 310 entries, and each `tvg-name` is the raw provider name.

Other defects in the same path:

- `GET /api/v1/channels` returns the raw `c.name` (`crates/api/src/lib.rs:4282`). Thus the UI never shows the output name.
- The XMLTV handler has a fallback that matches a programme to a channel by name (`crates/api/src/lib.rs:6831`). After an alias renames a channel, the fallback fails. The primary match by `channel_id` still works.
- The aliases table is a many-to-one normalization table. It is not a per-channel rename feature. See F3.

Fix (do not change `normalize_channel_name`):

- Expression indexes in migrations 0064 to 0068 use `normalize_channel_name`. EPG matching also uses it. A change to it requires a reindex and changes EPG results.
- Add a new idempotent migration with a new function `normalize_channel_alias_key(text)`. The function strips a prefix of two to four letters before `:` or `|`, then calls `normalize_channel_name`.
- Use the new function only in the alias lateral join of `list_enabled_channels_for_output_profile`. The output query reads only enabled channels (310 rows), so the join needs no index.
- Show the effective output name in `GET /api/v1/channels` as a new field `outputName`. Keep `name` as the provider name.

### F3. No per-channel rename exists

The code has no column, endpoint, or UI control for a per-channel name. `grep` for `name_override`, `display_name_override`, and `custom_name` returns no results.

A direct edit of `channels.name` does not persist. Provider reconciliation rewrites the name on each refresh:

```sql
ON CONFLICT (id) DO UPDATE SET
    name = EXCLUDED.name,          -- crates/persistence/src/catalog.rs:4516
```

Fix:

1. Add a nullable column `channels.name_override text` in a new migration with `ADD COLUMN IF NOT EXISTS`. Reconciliation must not write this column.
2. Use this precedence in the output query: `name_override`, then the alias canonical name, then `c.name`.
3. Add `PATCH /api/v1/channels/{channel_id}/name` with the body `{"nameOverride": "<text>" | null}`. A `null` value removes the override.
4. Add an inline rename control to the **Channels** tab.

Event channels need a related feature. Provider names such as `NHL: 01 - 7:30pm Senators @ Sabres [1080p]` change each day. WP4 and WP8b decide the event channel name. A manual override has precedence over the event name.

### F4. The event scan always produces zero event channels

Live evidence:

```text
GET /api/v1/event-templates   -> 3 enabled templates (mlb, nhl, sports)
GET /api/v1/event-channels    -> []
GET /api/v1/events            -> []
GET /api/v1/event-templates/suggestions -> 37 suggestions, 933 streams for "sports"
```

The provider does publish event times in many stream names:

```text
US (ESPN+ 069): Maryland Eastern Shore vs. High Point Nov 04 7:00PM ET (2026-11-04 19:00:25) [1080p]
CA (TSN+ 007): NCAA Football: Ball State vs. Massachusetts (2026-11-04 19:00:00) [1080p]
(FLSP 004): hockey:  Kingston Frontenacs vs Ottawa 67`s (Home) (2026-11-04 10:30:00) [1080p]
NHL: 01 - 7:30pm Senators @ Sabres [1080p]        (group "USA: NHL PACKAGE [1080p]", enabled)
NFL : 01 - [1080p]                                (empty slot, no event)
```

`scan_event_channels` (`crates/persistence/src/catalog.rs:3119`) has these six independent blockers. Each blocker alone is sufficient to produce zero results.

| # | Blocker | Evidence | Fix |
|---|---------|----------|-----|
| B1 | The template regex runs as a PostgreSQL `~` filter. All stored templates use `\b`. In a PostgreSQL ARE, `\b` is a backspace, not a word boundary. Thus no row matches. | `catalog.rs:3160`. The repository records this fact in `migrations/0007_normalize_channel_name.sql:35`. The suggestion builder writes `\b` into each template (`catalog.rs:4396`, `build_suggestion_config`). | Apply `matchRegex` in Rust with the `regex` crate, which the web UI and the docs assume. Keep a cheap SQL prefilter only. Repair stored templates in a data migration. |
| B2 | The built-in sports rules require the date at the end of the name. Live names add `:SS` seconds, `[1080p]`, `(Home)`, and other suffixes. The `QUALITY` part does not accept brackets. | `crates/domain/src/events.rs:191`, `:215`, `:225` | Accept optional seconds, bracketed tags, and parenthesized suffixes. Add a time-only form, see B2a. |
| B2a | Names in the NHL, NFL, NBA, and MLB packages have a time without a date (`7:30pm`). The built-in rules have no time-only form. | Live channel 9298 | Add a time-only rule. Resolve the date from the snapshot date in the source timezone. Confirm with ESPN, see F7. |
| B3 | All templates use `channelNameFormat: "{event}"`. `render_event_title` knows only `{source}`, `{title}`, and named captures. `{event}` returns `UnknownTemplateField`, and the scan skips the slot. | `crates/parsers/src/events.rs:622`; the scan copies the format at `catalog.rs:3176`; `let Ok(schedule) = ... else { continue }` at `catalog.rs:3230` | Add an `{event}` alias that renders the built-in rule title, for example `{away} @ {home}`. Change suggestions to use `{event}` with that meaning. Return the render error in the scan result. |
| B4 | The scan attaches all programmes to the first enabled channel in the group `template.group_name` (`"Sports"`). No live group has that name. All 310 enabled channels are in groups such as `USA: NHL PACKAGE [1080p]`. | `catalog.rs` scan, query `WHERE coalesce(group_name, 'Uncategorized') = $1 AND enabled ... LIMIT 1` | Attach each event to the channel that carries the matched provider stream, through `channel_streams`. Key `event_channels` by `(template_id, channel_id)`. |
| B5 | The slot regexes in SQL use `\\s` inside a Rust raw string. PostgreSQL receives `\\s`, which matches a literal backslash. Thus the slot number is almost always 1, and all events of a template collapse into one slot. | `catalog.rs:3142`, `:3146` | Remove the slot model. B4 replaces it with the channel identity. |
| B6 | The template timezone is `UTC`, and the source timezone is `UTC`. The provider times are US Eastern. ESPN confirms this fact: `Senators @ Sabres` at `7:30pm` is `2026-11-05T00:30Z`. | `GET /api/v1/sources` shows `"timezone": "UTC"` for the Xtream source. | Default the template timezone to the source timezone. Set the source timezone to `America/New_York` (F12). |

The scan also hides failures. It returns `"Scanned 0 event channels"` and does not report the rows that it skipped or the reason.

### F5. The event scan does not run automatically, and prune is incorrect

- `scan_all_event_channels` (`catalog.rs:3307`) has a doc comment that says "after provider reconciliation". No production code calls it. Only tests call it.
- The worker runs `finalize-provider-reconciliation` in `run_provider_reconciliation_finalizer_job` (`apps/server/src/main.rs:609`, `:1214`). That is the correct hook.
- Prune applies the grace of the selected template to every template (`crates/api/src/lib.rs:5688`, `:5689`). No process runs prune automatically.
- The code never sets `event_channels.state` to `live` or `ended`. The output query reads only `state = 'scheduled'` (`catalog.rs:2795`).

### F6. `GET /api/v1/events` has an unexpected shape

`list_events` returns `DynamicEventResponse` (`crates/api/src/lib.rs:617`, `:5366`) with the fields `group`, `rawTitle`, `programmeTitle`, `channelSlot`, `start`, `state`, and `template`. The web client decodes the response as `EventChannel[]` (`client.ts:750`). No page reads `getEvents()` now, so the defect has no visible effect yet. Make the client type agree with the server, or remove the unused client method.

### F7. Sports metadata enrichment does not exist

Requirement: give each real sports event the same metadata that the operator's Jellyfin post-processor makes:

- The matchup ("X at Y"), from the title.
- Home and away teams.
- Team logos.
- A matchup title and a matchup thumbnail.
- The start time.

Evidence that the ESPN scoreboard gives all of these values:

```text
GET https://site.api.espn.com/apis/site/v2/sports/hockey/nhl/scoreboard?dates=20261104
{"name":"Ottawa Senators at Buffalo Sabres","shortName":"OTT @ BUF","date":"2026-11-05T00:30Z",
 "comps":[{"homeAway":"home","abbr":"BUF","name":"Buffalo Sabres","short":"Sabres",
           "logo":"https://a.espncdn.com/i/teamlogos/nhl/500/scoreboard/buf.png"},
          {"homeAway":"away","abbr":"OTT","name":"Ottawa Senators","short":"Senators", ...}],
 "venue":"KeyBank Center","bc":["ESPN+","MSGB"]}
```

The live channel `NHL: 01 - 7:30pm Senators @ Sabres [1080p]` agrees with this event. The ESPN `date` agrees with `7:30pm` in US Eastern time.

Design for the start time, in priority order:

1. An explicit date and time in the name, for example `(2026-11-04 19:00:25)`. Interpret it in the source timezone and ignore the seconds. The provider uses the seconds to sort streams.
2. A time without a date, for example `7:30pm`. Use the snapshot date in the source timezone. If the result is more than 12 hours in the past, add one day.
3. An ESPN scoreboard match. Query the league for the local date and the next date. Match the two teams with the normalized `displayName`, `shortDisplayName`, `location`, and `abbreviation`. If the parsed time and the ESPN time differ by more than 2 hours, use the ESPN time and record the conflict.
4. No time and no ESPN match, for example `NFL : 01 - [1080p]`. Do not make an event programme. Publish filler only, or nothing.

League detection:

- Use the name prefix: `NHL:`, `NFL :`, `NBA:`, `MLB:`, `NCAA Football:`, `NCAA Women's Basketball:`.
- Use the group name: `USA: NHL PACKAGE`.
- Use the template name.
- Map each league to the ESPN `<sport>/<league>` path in a static table. Examples: `hockey/nhl`, `football/nfl`, `basketball/nba`, `baseball/mlb`, `football/college-football`, `basketball/mens-college-basketball`, `basketball/womens-college-basketball`.
- Leagues that ESPN does not cover use only the name parser, for example junior hockey and Division III soccer.

Home and away:

- `A @ B` and `A at B` mean that A is away and B is home.
- `A vs B` has no fixed meaning. Use ESPN `homeAway` when a match exists. Otherwise use the provider `(Home)` or `(Away)` tag when present. Otherwise keep the source order and do not assign home or away.

Caching and limits:

- Cache scoreboard responses for each league and date for 10 minutes.
- Cache team lists for 24 hours.
- Send a generic `User-Agent`.
- Use a 5 second timeout and a circuit breaker. An ESPN failure must not block the scan. The scan then uses only the parsed values.

Output metadata for each event programme:

- Title: `Ottawa Senators at Buffalo Sabres`.
- Sub-title: do not write it. Put the league and venue in the description. See F8.
- Description: the matchup, the broadcasters, and the source title.
- Categories: `Sports`, `Sports event`, and the league, for example `NHL`. Jellyfin uses the `Sports` category to classify a programme as sports.
- Programme icon: phase 1 uses the home-team logo URL. Phase 2 (WP9) serves a composite matchup image from the relay.
- `<live/>` and `<episode-num system="original-air-date">` with the local date.

Store the values in `generated_programmes.metadata` (jsonb) and `categories`. These columns exist already.

### F8. XMLTV output is incomplete

- The XMLTV handler writes `title`, `desc`, and `category` only (`crates/api/src/lib.rs:6836` to `:6851`). `ProgrammeRow` has `subtitle`, but the handler does not write it.
- The omission of `sub-title` is possibly intentional. The operator reports that `sub-title` caused Jellyfin crashes. The Git history has no record of this: the API XMLTV writer never wrote `sub-title` in any commit. Thus the code does not document the reason. Treat the omission as intentional until a test proves otherwise.
- Fix: do not add `sub-title` to the output. Add a code comment at the XMLTV handler that records the Jellyfin crash reason. Add a test that fails if the output contains `<sub-title>`. If a later package must add `sub-title`, first reproduce the crash with a test Jellyfin instance, and find the input that causes it.
- Generated programmes have the categories `["Dynamic event", kind]` (`catalog.rs`, `replace_generated_programmes`). Jellyfin does not classify them as sports.
- No programme `icon`, `episode-num`, or `live` element exists.

### F9. The runtime does not read operator settings

The schema has nine keys. `load_operator_setting_overrides` has two callers only: the effective-settings handler and the overrides list handler (`crates/api/src/lib.rs:2727`, `:2789`). No runtime code reads the effective values.

| Key | State |
|-----|-------|
| `media.ring.duration_seconds` | Fixed in v0.2.19 (`RingPolicy::from_settings`, commit `36b2c1f`). |
| `media.ring.max_bytes` | Fixed in v0.2.19. |
| `media.viewer.start_delay_seconds` | Fixed in v0.2.19 (ring pre-roll). |
| `media.recovery.window_seconds` | Not read. |
| `media.adapter` | Not read. The live instance has the global override `"media.adapter":"ffmpeg"`. The runtime uses the per-stream `input_adapter` column instead (`crates/api/src/lib.rs:7079`). |
| `ingest.max_download_bytes` | Not read. |
| `ingest.max_m3u_line_bytes` | Not read. |
| `events.inferred_duration_seconds` | Not read. Templates have their own duration. |
| `events.filler_title` | Not read. Templates have their own filler title. |

### F10. Short-EPG refresh fails, and guide coverage is low

- `/api/v1/system` reports `guideCoverage: 5.48` and `deadLetterCount: 17`.
- All 17 dead letters are `refresh-xtream-short-epg`. Thirteen have "HTTP source returned an unsuccessful status". Four have "source artifact contained no usable records".
- Only 17 of 310 enabled channels have an EPG mapping (`GET /api/v1/epg/mappings` total 17).

This finding is out of the requested scope. WP10 is an investigation package.

### F11. `providerLimit` is 0 when the instance is idle

`system_info_value` (`crates/api/src/lib.rs:2305` to `:2330`) counts capacity from media sessions. If no session exists, it reads the in-memory catalog. In database mode the in-memory catalog has no stream sources. Thus the UI shows `0` instead of `3`. Read `max_connections` from the database sources instead.

### F12. The source timezone is wrong

The Xtream source has `"timezone": "UTC"`. The provider names use US Eastern time (F4 B6). Operator action: set the source timezone to `America/New_York` in the **Sources** tab after WP4 makes the scan read it.

### F13. Jellyfin shows the wrong stream quality for relay channels

The parent session verified this finding on 2026-11-04 against relay v0.2.20.

Evidence:

- Real stream: a 60 second pull of channel 16968 (`CAN: SPORTSNET WEST [1080p]`) is 1280x720 H.264 at 29.97 fps, with AAC audio at about 212 kbps. The total is about 3.2 Mbps. Thus the `[1080p]` tag in the provider name is also wrong.
- Jellyfin's "Live stream opened" log for that channel reports 1920x1080, 59.94 fps, `BitRate 20000000`, a total bitrate of `20095812`, and a cap of `30000000`. These values are placeholders, not probe results.
- `/out/<token>/hdhr/lineup.json` gives each channel only `GuideName`, `GuideNumber`, `ImageURL`, and `URL` (`HdhrChannel` in `crates/api/src/lib.rs`, handler `hdhr_lineup` at line 7370).

Root cause: Jellyfin's HDHomeRun tuner (`HdHomerunHost.GetMediaSource`) uses `channelInfo.IsHD ?? true`. `IsHD` comes from the lineup `HD` field. `VideoCodec` and `AudioCodec` also come from the lineup. When the lineup has no `HD` field, Jellyfin gives each channel fixed HD values: 1080p and 20 Mbps in Jellyfin 10.x (12.4), or 15 Mbps on the current master branch. SD channels get about 2 Mbps.

Impact: Jellyfin uses these values for direct-play decisions, transcode decisions, and bandwidth limits. Thus Jellyfin can transcode or limit a client without a real need.

Data gap: the relay has columns for probe results on `provider_streams` (`video_codec`, `video_width`, `video_height`, `video_fps`, `audio_codec`, `bitrate_kbps`). The health-check job writes them through `update_stream_health_and_rank` (`apps/server/src/main.rs:1904`). But all 44,608 live streams have the health status `unknown`. Thus the relay has no real quality data now.

Fix options:

- **Option A (relay change, recommended).** Add `HD` (`1` or `0`), `VideoCodec`, and `AudioCodec` to each lineup entry. Use only measured stream data. Do not use the `[720p]` or `[1080p]` name tags. If no measured data exists for a channel, omit the three fields. Jellyfin then keeps its current default. To get measured data without extra provider connections, record the codec and resolution passively when a viewer opens a stream.
- **Option B (Jellyfin configuration only).** Add the relay to Jellyfin as an M3U tuner with `/out/<token>/playlist.m3u`, not as an HDHomeRun tuner. Jellyfin then probes the real stream when it opens it. This option needs no code change. Disadvantages: the operator must set up the tuner again, enter the 3-connection limit manually, and map the guide again.

Recommendation: use Option A. It keeps the current Jellyfin setup and gives correct values to all HDHomeRun clients. Option B is an immediate workaround that the operator can apply before Option A ships.

### F16. Event channels need guide data from their names

The low guide coverage (F10) is mostly correct for team and regional channels. But the event channels are a different case. Their names contain the event and the start time. Jellyfin cannot read this information from a name, so the relay must put it in the guide and the channel data that it serves.

Evidence from the served `/out/<token>/xmltv.xml` (310 channels, 683 programmes), captured 2026-11-04:

| Name format | Channels | With a programme | Example |
|-------------|----------|------------------|---------|
| Matchup, date, then league and slot | 12 | 0 | `Detroit Pistons vs Philadelphia 76ers @ Nov 4 7:00 PM :NBA  01 [1080p]` |
| League and slot, then time only | 5 | 0 | `NHL: 03 - 10pm Sharks @ Kings [1080p]` |
| Explicit date and time | 2 | 0 | `MLB 01:  Brewers x Rays start:2026-10-28 00:00:00 stop:2026-10-28 07:13:20 [1080p]` |
| Empty slot | 26 | 0 | `NHL: 05 - [1080p]` |
| Other (team, regional, national) | 265 | 17 | `USA: DETROIT SPORTSNET [1080p]` |

Defects:

1. The event scan (F4) produces no programme for these channels.
2. WP2 does not cover two of the formats:
   - In `A vs B @ Nov 4 7:00 PM :NBA 01`, the `@` separates the matchup from the date. It does not mean "at". The `vs` order does not give home and away, so ESPN must decide.
   - `MLB 01:  Brewers x Rays start:... stop:...` uses `x` as the separator, and it gives an explicit stop time.
3. The provider changes the channel name for each event. Thus Jellyfin shows a different channel name each day, and the slot number is the only stable part.
4. An empty slot has no programme. Jellyfin shows no guide data for it.
5. 293 of 310 channels have no programme at all. This includes the 265 team, regional, and national channels with no XMLTV match.

Operator rule (2026-11-04):

- If a channel has no time or event in its name, and no programme in XMLTV, keep the channel name.
- Serve the programme "No program available" for that channel until it has real programmes.
- Fill each gap in a channel's guide the same way.

Fix:

- WP2b: parse the two missing formats.
- WP4: run event detection on all enabled channels automatically, give each event channel a stable name, and add event filler programmes. The WP4 brief lists the details.
- WP14: fill each guide gap on each enabled channel with "No program available".

### F14. Bursty provider delivery stalls transcoding clients

The parent session measured this finding on 2026-11-04 against v0.2.20.

Evidence:

- The provider sends live data in bursts of 4 to 10 MB every 10 to 27 s, with no data between the bursts. All 3 active channels show this pattern.
- The measurement came from inside the relay. The parent polled `/api/v1/sessions` each second and compared `overwrittenPackets` plus `retainedPackets`. No proxy was in the path.
- Jellyfin's `SharedHttpStream` file and its HLS segments show the same pattern: 7 or 8 segments of 3 s each at one time, then gaps of up to 27 s.
- The relay adds no delay. `into_byte_stream` (`crates/media/src/session.rs`) sends 32-packet batches when they arrive. Caddy uses `flush_interval -1`.
- One remote viewer (Moonfin for Android, Pixel 7a, transcoded to about 3.8 Mbps) stalls about 15 s into playback, because the player runs out of data in a gap. Direct-play Roku and Android TV viewers report no problem.
- The provider startup burst is 30 to 45 s of media. A 60 s pull of channel 16968 returned 100 s of media.
- All 3 provider slots were in use by real Jellyfin viewers during the measurement.

Root cause: the relay forwards the provider bursts directly. A client with a small buffer, or a Jellyfin transcode that writes segments in real time, has no data during the gaps.

Fix: pace each viewer at real time from the PCR, and keep each viewer a fixed delay behind the ring head (WP12). The startup burst fills the delay, so the first viewer starts immediately.

Interaction with v0.2.19: `RingPolicy::from_settings` in `crates/api/src/lib.rs` already reads `media.viewer.start_delay_seconds` as the ring pre-roll, with a default of 2 s. WP12 changes the meaning to "paced delay behind the head" and changes the default to 30 s. The value 0 must keep the current unpaced behavior.

### F15. Stream start failures need slates

Current state:

- `capacity_placeholder_response` (`crates/api/src/lib.rs`) streams an ffmpeg `lavfi` slate with the text "No more slots available" (1280x720, 30 fps, libx264, AAC 96 kbit/s). It retries `try_open` every 500 ms, and then splices to the live stream.
- No test on a real client covers this splice.
- A provider error or a startup error returns an HTTP error. Jellyfin then fails to open the channel.

Operator goals:

- A stream must start immediately and never fail to open in Jellyfin.
- While the buffer fills (if necessary), show "Stream starting in N seconds" with a countdown.
- While the provider pool is full, show "All tuners in use, stay tuned for one to open up".
- On a provider error or a startup error, show "Having trouble reaching this channel, retrying". Do not return an HTTP error.

Main risk: Jellyfin probes the first seconds of the stream. It sets direct play or transcode from the slate parameters. If the live stream has a different resolution, frame rate, audio codec, audio channel count, or timestamp base, playback can fail after the splice.

Mitigations:

- Render each slate to agree with the last known stream profile of the channel (WP11b data). Use the same PIDs (`0x100` video, `0x101` audio).
- At the splice, set `discontinuity_indicator`, and send a new PAT and PMT. Keep PTS and PCR monotonic, or rebase them.
- Keep the audio layout identical between slate and live.

## 4. Work packages

### 4.1 Shared-file lanes

`crates/api/src/lib.rs` has 13,860 lines, and `crates/persistence/src/catalog.rs` has 7,452 lines. Most packages must edit one of them. Thus the plan uses lanes. Packages in one lane run in sequence. Packages in different lanes run in parallel, because they do not edit the same files.

| Lane | Files | Packages, in order |
|------|-------|--------------------|
| API | `crates/api/src/lib.rs` (except the stream handler functions), `tests/fixtures/openapi.json` | WP1, WP11a, WP14, WP5, WP7, WP8b, WP6 |
| Persistence | `crates/persistence/src/catalog.rs`, `crates/persistence/tests/postgres.rs`, new files in `migrations/` | WP3, WP11a, WP4, WP8b |
| Media | `crates/media/src/*` (including `session.rs` `into_byte_stream` and `ring.rs`), `apps/server/src/main.rs` (health-check code only) | WP11b, WP12, WP13, WP6 |
| Stream handlers | `crates/api/src/lib.rs` functions `stream_channel`, `open_channel_response`, `capacity_placeholder_response`, `RingPolicy`, and the passive capture hook | WP12, WP13, WP6 |
| Web | `apps/web/src/lib/api/client.ts`, `apps/web/src/lib/api/types.ts` | WP1, WP5, WP7 |
| Parser | `crates/domain/src/events.rs`, `crates/parsers/src/events.rs`, `tests/fixtures/events/` | WP2, WP2b |
| Sports | new crate `crates/sports/` | WP8a |
| Worker | `apps/server/src/main.rs` | WP4 |

Dependency on the v0.2.19 ring fix (status: shipped in v0.2.19 and v0.2.20, now on `main`): the session on `main` edits `crates/api/src/lib.rs` (router, `configuration_control_routes`, and the stream source spec near line 7000 to 7400) and `crates/media/src/session.rs`. No package in this plan edits `default_ring` or `into_byte_stream`. Start the API lane only after v0.2.19 is on `main`. Then rebase on it.

Migration numbers: the last migration is `0074`. The v0.2.19 diff adds no migration. Assign the numbers at merge time, in lane order: WP3 takes `0075` and `0076`, WP4 takes `0077`, WP8b takes `0078` and later. Check `ls migrations` before each merge. Follow the migration rules in `AGENTS.md`.

### 4.2 Execution order

```text
Wave 0: v0.2.19 ring fix (done, on main)
Wave 1 (done):     WP1, WP2, WP3, WP8a, WP10, WP11b  -> merged on claude/audit-integration
Wave 2a:           WP11a (API + Persistence; small)
                   WP2b (Parser; small; parallel with WP11a)
                   WP14 (API; small; after WP11a)
                   WP12 (Media + Stream handlers; parallel with WP11a, WP4, WP5)
Wave 2b (parallel): WP4 (Persistence + Worker; needs WP2b)   WP5 (API + Web)   WP12 continues
Wave 3 (parallel):  WP7 (API + Web; needs WP4, WP5)   WP13 (Media + Stream handlers; needs WP11b, WP12)
Wave 4:             WP8b (API + Persistence; needs WP4, WP7, WP8a)
Wave 5 (parallel):  WP6 (API + Media + Stream handlers; needs WP8b, WP12, WP13)   WP9 (optional; after WP8b)
```

### 4.3 Model guidance

| Package | Model | Reason |
|---------|-------|--------|
| WP1 | Haiku | Mechanical attribute and test changes. The plan lists each struct. |
| WP2 | Sonnet | Regex and parser work with clear fixtures. |
| WP2b | Sonnet | Two more title formats, with the same fixture approach as WP2. |
| WP14 | Haiku | Small gap-fill function in the XMLTV output, with clear rules and tests. |
| WP3 | Sonnet | The plan gives the schema and the query change. |
| WP4 | Opus | Data model change, scheduler hook, and data repair. It crosses persistence, worker, and parser. |
| WP5 | Sonnet | One endpoint and one UI control, to a clear specification. |
| WP6 | Sonnet | The operator decided to connect all keys, and v0.2.19 gives a pattern to copy. Escalate to Opus if `media.recovery.window_seconds` needs changes in the session recovery logic. |
| WP7 | Sonnet | Handler and UI changes to a clear specification. |
| WP8a | Sonnet | New pure crate with fixtures. Section F7 gives the matching rules. |
| WP8b | Sonnet | Integration to the F7 specification. Escalate to Opus if Jellyfin parses the XMLTV elements differently from the specification. |
| WP9 | Sonnet | Image composition with a known library. |
| WP10 | Sonnet | Read-only investigation. |
| WP11a | Haiku | Small, mechanical change to the lineup JSON, with a clear specification and tests. |
| WP11b | Sonnet | Passive capture of stream data in the media path. It must not open extra provider connections. |
| WP12 | Sonnet | Medium size, contained in the media crate, with a clear specification and fake-clock tests. |
| WP13 | Opus | Large and risky. Splices must work on several real clients and on the Jellyfin transcoder. It needs design judgment and on-device test planning. |

### 4.4 Package briefs

Each brief is a complete start prompt for a child session. Each child session must follow `AGENTS.md`: STE for documentation, the migration rules, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and the web checks when it changes web code. This Mac has no Docker. Thus `./scripts/run-test-suite.sh` and the PostgreSQL tests in `crates/persistence/tests/postgres.rs` run only in CI or on a host with Docker. Each child session must say in its report which tests it could not run.

#### WP1. API JSON casing (Haiku)

- Findings: F1.
- Files: `crates/api/src/lib.rs` (struct definitions and test bodies only), `tests/fixtures/openapi.json`, `apps/web/src/lib/api/client.ts` (one line), and web tests that cover these calls.
- Depends on: v0.2.19 on `main`.

Brief:

> In `crates/api/src/lib.rs`, add `#[serde(rename_all = "camelCase")]` to these structs: `StreamHealthItem`, `BestStreamResponse`, `UserResponse`, `CreateUserRequest`, `UpdateUserRequest`, `ChannelAliasResponse`, `CreateChannelAliasRequest`, `ResolveAliasResponse`, `RecordingRuleResponse`, `CreateRecordingRuleRequest`, `RecordingResponse`, `CreateRecordingRequest`, `RecordingStatsResponse`, `StreamProfileResponse`, `CreateStreamProfileRequest`, `AssignStreamProfileRequest`. Do not change `DeadLetterResponse`. In `apps/web/src/lib/api/client.ts`, change `assignStreamProfile` to send `{ streamProfileId }`. Update each Rust test that sends or asserts a snake_case key for these resources. Add a unit test that iterates `ApiDoc::openapi()` component schemas and fails on a property name with `_`, with an allow list that contains `DeadLetterResponse`. Run `make openapi-snapshot` and commit the new snapshot. Done when `cargo test -p iptv-api` and the web checks pass, and the five tabs show data against a local server.

Test plan:

- The new OpenAPI casing guard test.
- The existing route tests with camelCase bodies.
- After deployment, run `GET /api/v1/channel-aliases`, `/stream-profiles`, `/streams/health`, and `/recordings/stats`, and confirm camelCase keys.
- After deployment, create and delete one alias in the UI.

#### WP2. Event title parser (Sonnet)

- Findings: F4 B2, B2a, B3.
- Files: `crates/domain/src/events.rs`, `crates/parsers/src/events.rs`, `tests/fixtures/events/`.
- Depends on: nothing.

Brief:

> Make the built-in sports rules in `crates/domain/src/events.rs` (`builtin_sports_event_rules`) match the provider titles in `tests/fixtures/events/`. Add a fixture file with these live examples and their expected values (teams, separator, date and time, tags):
> `US (ESPN+ 069): Maryland Eastern Shore vs. High Point Nov 04 7:00PM ET (2026-11-04 19:00:25) [1080p]`,
> `CA (TSN+ 007): NCAA Football: Ball State vs. Massachusetts (2026-11-04 19:00:00) [1080p]`,
> `(FLSP 004): hockey:  Kingston Frontenacs vs Ottawa 67`s (Home) (2026-11-04 10:30:00) [1080p]`,
> `(FLSP 013): fc:  Penn State Behrend vs La Roche   Men`s (PSU Behrend vs La Roche) (2026-11-04 14:00:05) [1080p]`,
> `NHL: 01 - 7:30pm Senators @ Sabres [1080p]`, and `NFL : 01 - [1080p]` (expect no match).
> Accept optional `:SS` seconds, bracketed tags such as `[1080p]`, parenthesized tags such as `(Home)`, and a prefix such as `CA (TSN+ 007): NCAA Football:`. Add a time-only rule (`7:30pm`, `10pm`) that captures `time` without `date`. The caller resolves the date. Expose the date resolution as a pure function: it takes a time, a reference instant, and a timezone. Capture `league` when a known league prefix exists. In `render_event_title` (`crates/parsers/src/events.rs`), support `{event}` as the rule's default title, for example `{away} @ {home}`. Do not change the public signatures that `crates/persistence` uses, unless you also list the change in your report for WP4. Done when the new fixture tests and the existing parser tests pass.

Test plan: unit tests on each fixture title, plus DST edge cases for the time-only resolver in `America/New_York`.

#### WP3. Alias matching and name override storage (Sonnet)

- Findings: F2, F3 (storage layer).
- Files: new migrations `0075_channel_alias_key.sql` and `0076_channel_name_override.sql`, `crates/persistence/src/catalog.rs` (`list_enabled_channels_for_output_profile`, `list_channels`, a new `set_channel_name_override`, and the `ChannelRow` struct), `crates/persistence/tests/postgres.rs`.
- Depends on: nothing.

Brief:

> Do not change `normalize_channel_name`; EPG expression indexes depend on it. In a new idempotent migration, add `CREATE OR REPLACE FUNCTION normalize_channel_alias_key(raw text)`. The function removes a leading `^[A-Za-z]{2,4}\s*[:|]\s*` prefix and returns `normalize_channel_name` of the rest. In a second migration, add `ALTER TABLE channels ADD COLUMN IF NOT EXISTS name_override text`. In `list_enabled_channels_for_output_profile`, change the alias lateral join to compare `normalize_channel_alias_key(...)` on both sides. Select `coalesce(c.name_override, channel_alias.canonical_name, c.name) AS name`. Also return the raw provider name as `provider_name`. In `list_channels`, return the same effective name as a new `output_name` field. Add `set_channel_name_override(channel_id, Option<&str>)`. It trims the value, stores `NULL` for an empty value, and rejects more than 200 characters. Confirm that the reconciliation upsert near `catalog.rs:4516` does not write `name_override`. Add PostgreSQL tests: (1) alias `NHL Network` renames `USA: NHL NETWORK [1080p]` in the output; (2) an override has precedence over an alias; (3) a reconciliation run keeps the override. Done when `cargo clippy` passes and the PostgreSQL tests pass in CI.

Test plan: the three PostgreSQL tests above, plus a migration rerun test that applies the two new migrations twice.

#### WP2b. Additional event title formats (Sonnet)

- Findings: F16.
- Files: `crates/domain/src/events.rs`, `crates/parsers/src/events.rs`, `tests/fixtures/events/live-titles.json`.
- Depends on: WP2 (done). Start from `claude/audit-integration`.

Brief:

> Extend the built-in sports rules from WP2 for two more live formats. Read section F16 of `docs/audit-plan.md` first.
> (1) `Detroit Pistons vs Philadelphia 76ers @ Nov 4 7:00 PM :NBA  01 [1080p]`: the `@` separates the matchup from the date, and it does not mean "at". Capture both teams, the separator `vs` (no home or away), the month, day, time, the league `NBA`, and the slot `01`. Resolve the year from the reference date.
> (2) `MLB 01:  Brewers x Rays start:2026-10-28 00:00:00 stop:2026-10-28 07:13:20 [1080p]`: capture the league, the slot, both teams with the separator `x` (no home or away), the start, and the stop.
> (3) For all rules, capture `league` and `slot` when the name has them, because WP4 makes the stable channel name from them.
> Add fixtures with the expected values for each format. Make sure that `A @ B` in the WP2 formats still means "A at B". Write each new capture name in the status file for WP4. Done when the fixture tests, fmt, and clippy pass.

Test plan: fixture tests for both formats, and the WP2 fixtures unchanged.

#### WP14. "No program available" guide placeholder (Haiku)

- Findings: F16 (operator rule).
- Files: `crates/api/src/lib.rs` (the XMLTV handler `xmltv` and a new gap-fill helper), tests in the same file.
- Depends on: WP11a (API lane). Start from `claude/audit-integration`.

Brief:

> In the XMLTV output (`/out/<token>/xmltv.xml`), make sure that each enabled channel has a programme for each minute of the guide window. Read section F16 of `docs/audit-plan.md` first.
> (1) Compute the guide window from the existing programme query range (default: 6 hours in the past to 48 hours in the future, if the handler has no range).
> (2) For each channel, sort its programmes, and fill each gap of 1 minute or more with a programme titled "No program available". Split long gaps into blocks of at most 2 hours, aligned to the hour.
> (3) Add `<category>` "No program" to the placeholder, and no description. Do not change the channel name.
> (4) Do not store placeholders in the database. Generate them only in the output.
> (5) A real programme always has precedence. If WP4 or a later refresh adds programmes, the placeholders for that time disappear.
> Add tests: a channel with no programmes gets placeholders for the full window; a channel with one programme gets placeholders before and after it; adjacent programmes give no placeholder. Done when the tests, fmt, and clippy pass.

Test plan:

- Unit tests as listed.
- After deployment: in Jellyfin, refresh the guide data, and confirm that a team channel shows "No program available".

#### WP4. Event scan engine and scheduling (Opus)

- Findings: F4 B1, B4, B5, B6, and F5 (scan and scheduling).
- Files: `crates/persistence/src/catalog.rs` (`scan_event_channels`, `scan_all_event_channels`, `upsert_event_channel`, `prune_past_event_channels`, `suggest_event_templates`, `build_suggestion_config`), new migration `0077_event_channels_by_channel.sql`, `apps/server/src/main.rs` (`run_provider_reconciliation_finalizer_job`), `crates/persistence/tests/postgres.rs`, `docs/docs/configuration/dynamic-events.md`.
- Depends on: WP2, WP2b, and WP3.

Brief:

> Rebuild `scan_event_channels` so that it produces event programmes for live provider names. Read section F4 of `docs/audit-plan.md` first.
> (1) Apply `matchRegex` with the Rust `regex` crate, not PostgreSQL `~`. Use a SQL prefilter only on enabled channels: join `provider_streams` to `channel_streams` and `channels` where `channels.enabled`.
> (2) Attach each event to the channel that carries the matched stream. Key `event_channels` by `(template_id, channel_id)`. Add a unique index in the new migration with `CREATE UNIQUE INDEX IF NOT EXISTS`. The table is small, so it needs no `CONCURRENTLY`. Keep `slot_number` for compatibility, but stop the use of the slot regexes.
> (3) Resolve the timezone in this order: template timezone if not `UTC`, then source timezone, then region timezone.
> (4) In the same migration, repair stored templates in an idempotent way: replace `\b` with `\y` in `match_regex`. Change `build_suggestion_config` so that it writes Rust-regex patterns, because step (1) uses the Rust regex engine. Decide and document which regex dialect `matchRegex` uses. Then make the repair agree with that decision.
> (5) Return a scan report with the matched, scheduled, and skipped counts, and the skip reasons. Keep `SaveResult.message` for compatibility.
> (6) Call `scan_all_event_channels` and then prune for each template, with that template's grace, at the end of `run_provider_reconciliation_finalizer_job`. Log errors. Do not fail the job because of a scan error.
> (7) Set `state` from the time: `scheduled` before the start, `live` during the event, `ended` after it, and `hidden` after the grace. Change the output query to include `live`.
> (8) Section F16: detect events on all enabled channels automatically. Use the built-in sports rules even when no operator template matches. Add a built-in template that is enabled by default, or an equal code path. Each refresh must give a programme to each of the 19 event channels in F16 without operator setup.
> (9) Give each channel with a parsed event a stable output name from the league and slot, for example `NBA 01` or `NHL 03`. Use this precedence: `name_override`, then the alias, then the stable event name, then the provider name. A channel with no parsed event (for example an empty slot) keeps its provider name (operator rule in F16). Keep the channel id and `tvg-id` stable. Record in the status file whether the name change between an empty slot and an event confuses Jellyfin.
> (10) Add event filler programmes on event channels. Before the event: "Upcoming: <title>, <local start time>". After the event: "Event ended". Do not add filler to empty slots; WP14 gives them "No program available". Use `events.filler_title` for the event filler text when WP6 connects it.
> (11) Use the explicit stop time when the name gives one (`stop:`). Otherwise use the template duration or `events.inferred_duration_seconds`.
> (12) Count generated programmes in the guide coverage value of `/api/v1/system`.
> Do not edit `crates/api/src/lib.rs`, except for item (12); WP7 owns the other handler changes. Update `docs/docs/configuration/dynamic-events.md` in STE. Done when a PostgreSQL test with the live titles from WP2 and WP2b produces event programmes on the correct channels at the correct UTC times, the stable names are correct, the filler fills each gap, and a second scan run is idempotent.

Test plan:

- PostgreSQL tests with the live titles.
- A test that runs the scan twice and checks for no duplicate programmes.
- A test that checks the template repair migration on a `\b` template.
- After deployment: `GET /api/v1/event-channels` is not empty after the next refresh. The XMLTV programme for channel `NHL: 01` starts at `00:30Z`.

#### WP5. Channel rename API and UI (Sonnet)

- Findings: F2 (UI visibility) and F3 (API and UI).
- Files: `crates/api/src/lib.rs` (`guide_control_routes`, `list_channels` response, new `set_channel_name_override` handler, XMLTV fallback at line 6831), `tests/fixtures/openapi.json`, `apps/web/src/lib/api/client.ts`, `apps/web/src/lib/api/types.ts`, `apps/web/src/pages/channels-page.tsx`, `docs/docs/configuration/channel-aliases.md`, `docs/docs/api/channels.md`.
- Depends on: WP1 and WP3.

Brief:

> Add `PATCH /api/v1/channels/{channel_id}/name` with the body `{"nameOverride": string | null}` and the `camelCase` serde attribute. Require the admin mutation check. Call `set_channel_name_override` from WP3. Return the updated channel. Add `outputName` and `nameOverride` to the `GET /api/v1/channels` items. In the XMLTV handler, match the fallback on `provider_name`, not on the output name. Add the client method and types. In the **Channels** tab, show `outputName` as the main label and the provider name as a second line. Add an inline rename control with a **Reset** action. Write a short "Rename a channel" section in `channel-aliases.md` in STE. Explain the precedence: override, then alias, then provider name. Regenerate the OpenAPI snapshot. Done when the API tests, the web checks, and a Vitest test for the rename control pass.

Test plan:

- An API route test.
- A Vitest component test.
- After deployment: rename one channel, then confirm the new name in `playlist.m3u`, `hdhr/lineup.json`, and `xmltv.xml`.

#### WP6. Operator settings wiring (Sonnet)

- Findings: F9 (except the ring keys) and F11.
- Files: `crates/domain/src/config.rs`, `crates/api/src/lib.rs` (state and startup, `system_info_value`), `crates/media/src/recovery.rs`, `crates/ingest/src/*` (limits), `apps/server/src/main.rs` (if the worker reads ingest limits).
- Depends on: WP8b (API lane), WP12 and WP13 (they edit the same media and stream functions). WP12 owns the `media.viewer.start_delay_seconds` meaning and default; do not change them.

Brief:

> Operator decision (2026-11-04): make every operator setting work. Do not remove a key. v0.2.19 connected `media.ring.duration_seconds`, `media.ring.max_bytes`, and `media.viewer.start_delay_seconds` through `RingPolicy::from_settings` in `crates/api/src/lib.rs` (commit `36b2c1f`). Read that code and use the same pattern for the six other keys. Respect each key's `applyRequirement` (`immediate` or `reimport`).
> - `media.recovery.window_seconds`: use it as the recovery window in `crates/media/src/recovery.rs`.
> - `media.adapter`: use it as the default input adapter for streams that have no explicit `input_adapter`. A per-stream value has precedence.
> - `ingest.max_download_bytes` and `ingest.max_m3u_line_bytes`: pass them to the ingest limits for each source refresh.
> - `events.inferred_duration_seconds` and `events.filler_title`: use them as defaults when a template has no value of its own.
> Also fix F11: compute `providerLimit` from the database sources when no session is active. Do not change `into_byte_stream` or the ring code. Done when each of the six keys has a test that proves a changed override changes the runtime behavior.

Test plan: one test for each connected key, and one API test for `providerLimit` with no session.

#### WP7. Events API and UI (Sonnet)

- Findings: F5 (prune handler), F6, and the UI for the WP4 scan report.
- Files: `crates/api/src/lib.rs` (`list_events`, `scan_event_channels`, `prune_event_channels` handlers, `DynamicEventResponse`), `tests/fixtures/openapi.json`, `apps/web/src/lib/api/client.ts`, `apps/web/src/lib/api/types.ts`, `apps/web/src/pages/events-page.tsx`, `docs/docs/api/events.md`.
- Depends on: WP4 and WP5.

Brief:

> Make the event handlers agree with the WP4 persistence API. The prune handler must use each template's own grace. The scan handler must return the WP4 scan report with matched, scheduled, and skipped counts and reasons. Make the `getEvents()` client type agree with `DynamicEventResponse`, or remove the unused method. In the **Events** tab, show the scan report after **Scan**. When a template has zero matches, show the skip reasons. Show the target channel name and the start time in local time for each event channel. Regenerate the OpenAPI snapshot. Update `docs/docs/api/events.md` in STE. Done when the API tests, the web checks, and a Vitest test for the scan report pass.

Test plan: API route tests for scan, prune, and list, and a Vitest test for the scan report display.

#### WP8a. ESPN client and matcher crate (Sonnet)

- Findings: F7.
- Files: new crate `crates/sports/` and the workspace `Cargo.toml` `members` entry. `Cargo.lock` changes as a result.
- Depends on: nothing. This crate must not depend on persistence or API crates.

Brief:

> Create the workspace crate `crates/sports` (`iptv-sports`). Implement the ESPN lookup that section F7 of `docs/audit-plan.md` describes:
> (1) A static table from league keys (NHL, NFL, NBA, MLB, NCAAF, NCAAM, NCAAW, WNBA, MLS) to ESPN `<sport>/<league>` paths.
> (2) An async client for `https://site.api.espn.com/apis/site/v2/sports/<sport>/<league>/scoreboard?dates=YYYYMMDD` and `/teams`. Use the existing `reqwest` workspace dependency, a 5 second timeout, a generic `User-Agent`, and an in-memory TTL cache (scoreboard 10 minutes, teams 24 hours).
> (3) A pure matcher: input is the league, two team strings, the separator (`@`, `at`, `vs`), an optional parsed start time, and a local date. Output is the ESPN event id, the UTC start time, the home and away teams (name, short name, abbreviation, logo URL), the venue, the broadcasters, and a confidence value. Match on normalized `displayName`, `shortDisplayName`, `location`, `name`, and `abbreviation`. Search the local date and the next date. If times disagree by more than 2 hours, prefer ESPN and set a conflict flag.
> Put recorded ESPN JSON in `crates/sports/tests/fixtures/` and test the matcher offline. Gate one live smoke test behind `#[ignore]`. Do not use the network in default tests. Done when `cargo test -p iptv-sports` passes offline and `cargo deny check` passes.

Test plan: offline fixture tests for these cases: `Senators @ Sabres` gives BUF home and a start of `00:30Z`, a `vs` title with an unknown order, a missing team, and a time conflict. Run the ignored live smoke test by hand.

#### WP8b. Sports metadata integration and XMLTV output (Sonnet)

- Findings: F7 and F8.
- Files: `crates/persistence/src/catalog.rs` (`replace_generated_programmes`, `list_programmes_for_channels`, `ProgrammeRow`), new migration `0078_*` (only if a column is necessary), `crates/api/src/lib.rs` (XMLTV handler), `apps/server/src/main.rs` (pass the sports client to the scan), `tests/fixtures/openapi.json` if a response changes.
- Depends on: WP4, WP7, and WP8a.

Brief:

> Connect `iptv-sports` to the WP4 scan. For each matched event with a known league, call the matcher, and store the result in `generated_programmes.metadata`. Set the title to `<Away full name> at <Home full name>`, and use `categories = ["Sports", "Sports event", <league>]`. An ESPN failure must not stop the scan. Return programme metadata from `list_programmes_for_channels`. In the XMLTV handler, write these elements in DTD order: `<desc>` (matchup, league, venue, broadcasters), each `<category>`, `<icon src="...">` (home-team logo), `<episode-num system="original-air-date">YYYY-MM-DD</episode-num>`, and `<live/>`. Before you finish, check how Jellyfin's XMLTV parser reads `icon`, `category`, `episode-num`, and `live`, and record the result in the report. If Jellyfin does not use an element, say so. Do not write `<sub-title>`: the operator reports that it caused Jellyfin crashes. Add a code comment with that reason and a test that fails if the output contains `<sub-title>`. Done when an XMLTV snapshot test for one NHL event passes, and the output passes `xmllint` against the XMLTV DTD.

Test plan:

- An XMLTV snapshot test.
- A PostgreSQL test that the metadata is stored.
- After deployment: in Jellyfin, refresh the guide data, and confirm that the `NHL: 01` programme shows as sports with a logo.

#### WP9. Matchup thumbnail (optional, Sonnet)

- Findings: F7 (phase 2).
- Files: new route `/out/{token}/art/{event_key}.png` in `crates/api/src/lib.rs`, an image module in `crates/sports`, and the XMLTV icon URL.
- Depends on: WP8b.

Brief:

> Make a 16:9 PNG that shows the away logo, "at", and the home logo on a neutral background, like the operator's Jellyfin post-processor. Fetch the logos through the WP8a cache. Cache the rendered images on disk with a size limit. Serve them through the output token route without other authentication. Change the XMLTV `<icon>` to this URL. Done when a snapshot test of the image dimensions passes and the route rejects an invalid token.

#### WP10. Short-EPG failure investigation (Sonnet, read-only)

- Findings: F10.
- Files: none. The package writes a report to the notes folder only.
- Depends on: nothing.

Brief:

> Investigate why all 17 `refresh-xtream-short-epg` jobs are dead letters ("HTTP source returned an unsuccessful status" and "source artifact contained no usable records"). Investigate why only 17 of 310 enabled channels have an EPG mapping. Use only `GET` requests against the live instance (`/api/v1/dead-letters`, `/api/v1/jobs`, `/api/v1/epg/*`), and read the code in `apps/server/src/main.rs` and `crates/ingest`. Do not open streams and do not call provider endpoints. Write the root cause and a proposed fix package. Do not change code.

#### WP11a. HDHomeRun lineup quality fields (Haiku)

- Findings: F13 (Option A, lineup part).
- Files: `crates/api/src/lib.rs` (`HdhrChannel`, `hdhr_lineup`, lineup tests), `crates/persistence/src/catalog.rs` (`list_enabled_channels_for_output_profile` and its row struct).
- Depends on: WP1 and WP3, because they edit the same files.

Brief:

> In `list_enabled_channels_for_output_profile`, also return `video_codec`, `video_height`, and `audio_codec` of the channel's primary provider stream (the `channel_streams` row with the lowest priority value). Use the `provider_streams` columns that exist. Add a migration only if a column is missing. Add three optional fields to `HdhrChannel` with these JSON names: `HD`, `VideoCodec`, `AudioCodec`. Use `#[serde(skip_serializing_if = "Option::is_none")]` for each field. Set `HD` to `1` when `video_height >= 720`, `0` when it is lower, and omit it when it is unknown. Map codecs to HDHomeRun values: `h264` to `H264`, `hevc` to `HEVC`, `mpeg2video` to `MPEG2`, `aac` to `AAC`, `ac3` to `AC3`, `eac3` to `EAC3`, `mp2` to `MPEG`. Omit unknown codecs. Do not read the quality tag in the channel name. Add tests: (1) a 720p H.264 and AAC stream gives `"HD":1,"VideoCodec":"H264","AudioCodec":"AAC"`; (2) a 480p stream gives `"HD":0`; (3) a stream without data gives none of the three keys. Done when the tests and `cargo clippy` pass.

Test plan:

- Unit tests for the JSON shape.
- After deployment, and after WP11b gives data: in Jellyfin, refresh the tuner, open channel 16968, and confirm that the "Live stream opened" log shows `IsHD` false or a lower bitrate.
- Before WP11b: confirm that the lineup output does not change for channels without data.

#### WP11b. Passive stream data capture (Sonnet)

- Findings: F13 (Option A, data part).
- Files: `crates/media/src/*` (session start path, outside `into_byte_stream` and `default_ring`), `apps/server/src/main.rs` (health-check code only), `crates/persistence/src/catalog.rs` (one new update method, or a call to `update_stream_health`).
- Depends on: v0.2.19 on `main`. It can run at the same time as WP11a.

Brief:

> Relay lineups need real codec and resolution data for each provider stream, but all 44,608 streams have the health status `unknown`. The provider allows only 3 connections, so do not add probes that open new provider connections. First find out why the health-check job (`apps/server/src/main.rs`, near `update_stream_health_and_rank` at line 1904) never stores results. Report whether it is disabled, always skipped, or failing. Then add passive capture: when a viewer session opens a provider stream, read the codec, width, height, frame rate, and audio codec from the first part of the MPEG-TS data that the session already receives. Store the values with `update_stream_health`. Store them at most one time per stream in each 24 hours. Do not change `into_byte_stream` or `default_ring`. Do not set the health status to `alive` from passive data unless the existing rules allow it. Done when a test with a recorded 720p H.264 and AAC TS sample stores the correct values, and the session test suite passes.

Test plan:

- A unit test with a short recorded TS sample in `tests/fixtures/`.
- After deployment, play one channel, then run `GET /api/v1/streams/health` and confirm that the stream has codec and resolution data.

#### WP12. Paced viewer buffer (Sonnet)

- Findings: F14.
- Files: `crates/media/src/session.rs` (`into_byte_stream`, viewer cursor), `crates/media/src/ring.rs`, a new `crates/media/src/pacing.rs`, `crates/api/src/lib.rs` (`RingPolicy` and the session diagnostics response only), `tests/fixtures/openapi.json` if the diagnostics change.
- Depends on: Wave 1 (done). Coordinate with WP6 and WP13: they edit the same functions. WP12 goes first.

Brief:

> Pace each viewer's output at real time, and keep each viewer a target delay behind the ring head. Read section F14 of `docs/audit-plan.md` first.
> (1) Find the PCR PID from the PMT. Pace the output by PCR against a monotonic clock. Handle PCR wrap (33-bit base), discontinuities, and generation boundaries (a new generation resets the PCR reference). If no PCR exists, fall back to pacing by the measured byte rate.
> (2) Target delay: `media.viewer.start_delay_seconds`. Change its default to 30 s. The value 0 must keep the current unpaced behavior exactly.
> (3) The first viewer starts immediately. The provider startup burst (30 to 45 s) fills the delay; pacing holds it back. If the burst is shorter than the target, start with the data that exists.
> (4) A late joiner gets a pre-roll equal to the delay from the ring. It must not open a new provider connection.
> (5) Size the ring for at least `max(60 s, 2 x delay)` of media by duration, so 1080p60 at 8 Mbps or more fits. Keep the `media.ring.max_bytes` cap and the 32 MiB floor from v0.2.19.
> (6) Add a `leadSeconds` value for each viewer to the session diagnostics.
> (7) Tests with a fake clock: pacing timing, PCR wrap, generation boundary, delay 0 keeps the current behavior, and late-joiner pre-roll.
> Do not change the lag recovery from v0.2.19 (skip to the next PAT/PMT boundary). Done when the tests pass, fmt and clippy pass, and the status file lists the changed settings default.

Test plan:

- Fake-clock unit tests.
- After deployment: watch one channel on the Moonfin Android client with transcoding for 10 minutes, and confirm no stall. Check `leadSeconds` in `/api/v1/sessions` (GET only).

#### WP13. Stream slates (Opus)

- Findings: F15.
- Files: `crates/api/src/lib.rs` (`stream_channel`, `open_channel_response`, `capacity_placeholder_response`), `crates/media/src/session.rs` (splice path), a new `crates/media/src/slate.rs`.
- Depends on: WP11b (stream profile data) and WP12 (paced output). Coordinate with WP6.

Brief:

> Make each channel open immediately in Jellyfin, and never return an HTTP error for a provider problem. Read section F15 of `docs/audit-plan.md` first.
> Add three slates: "Stream starting in N seconds" with a countdown (ffmpeg `drawtext` with `eif`) while the WP12 buffer fills, if necessary; "All tuners in use, stay tuned for one to open up" while the pool is full; and "Having trouble reaching this channel, retrying" for provider and startup errors.
> Render each slate to agree with the last known stream profile of the channel (resolution, frame rate, video codec, audio codec, channels, sample rate) from the WP11b data. Use a safe default when no data exists. Use PIDs `0x100` and `0x101`.
> At the splice from slate to live: set `discontinuity_indicator`, send a new PAT and PMT, and keep PTS and PCR monotonic (rebase if necessary). Keep the audio layout identical.
> Do not open more provider connections than the pool allows. Keep the existing 500 ms `try_open` retry, and add a back-off for provider errors.
> Write a manual acceptance plan for each slate-to-live transition on Roku, Jellyfin Android TV, Moonfin for Android, the Jellyfin web client, and the Jellyfin HLS transcoder. Done when unit tests cover slate selection and splice packet rules, and the operator completes the manual plan.

Test plan:

- Unit tests for slate selection and splice packets (PAT/PMT, discontinuity flag, monotonic PCR).
- Manual on-device tests from the acceptance plan. The provider has only 3 connections, so schedule them when no other viewer is active.

## 5. Verification after all packages

1. Run `./scripts/run-test-suite.sh` on a host with Docker.
2. Deploy, then run the live checks in each package test plan.
3. Confirm these results against the live instance:
   - `GET /api/v1/event-channels` is not empty after a refresh.
   - The XMLTV output has `Sports` programmes for the enabled NHL, NBA, and NFL event channels at the ESPN start times.
   - A channel rename shows in all three output files.
   - The **Channel aliases**, **Stream health**, **Stream profiles**, **Recordings**, and **Users** tabs show data and accept their forms.
