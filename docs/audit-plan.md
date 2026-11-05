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
| F8 | XMLTV output drops `sub-title` and does not mark sports programmes. | Medium | WP8b |
| F9 | The runtime does not read any of the nine operator settings. | Medium | WP6, and the v0.2.19 ring fix |
| F10 | Short-EPG refresh jobs fail. Guide coverage is 5.5 %. | Medium | WP10 |
| F11 | `/api/v1/system` reports `providerLimit: 0` when no session is active. | Low | WP6 |
| F12 | The Xtream source timezone is `UTC`, but the provider publishes US Eastern times. | Medium | Operator action, WP4 |

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
- Sub-title: league and venue.
- Description: the matchup, the broadcasters, and the source title.
- Categories: `Sports`, `Sports event`, and the league, for example `NHL`. Jellyfin uses the `Sports` category to classify a programme as sports.
- Programme icon: phase 1 uses the home-team logo URL. Phase 2 (WP9) serves a composite matchup image from the relay.
- `<live/>` and `<episode-num system="original-air-date">` with the local date.

Store the values in `generated_programmes.metadata` (jsonb) and `categories`. These columns exist already.

### F8. XMLTV output is incomplete

- The XMLTV handler writes `title`, `desc`, and `category` only (`crates/api/src/lib.rs:6836` to `:6851`). `ProgrammeRow` has `subtitle`, but the handler drops it.
- Generated programmes have the categories `["Dynamic event", kind]` (`catalog.rs`, `replace_generated_programmes`). Jellyfin does not classify them as sports.
- No programme `icon`, `episode-num`, or `live` element exists.

### F9. The runtime does not read operator settings

The schema has nine keys. `load_operator_setting_overrides` has two callers only: the effective-settings handler and the overrides list handler (`crates/api/src/lib.rs:2727`, `:2789`). No runtime code reads the effective values.

| Key | State |
|-----|-------|
| `media.ring.duration_seconds` | The v0.2.19 session on `main` fixes this key now. |
| `media.ring.max_bytes` | The v0.2.19 session on `main` fixes this key now. |
| `media.viewer.start_delay_seconds` | Not read. |
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

## 4. Work packages

### 4.1 Shared-file lanes

`crates/api/src/lib.rs` has 13,860 lines, and `crates/persistence/src/catalog.rs` has 7,452 lines. Most packages must edit one of them. Thus the plan uses lanes. Packages in one lane run in sequence. Packages in different lanes run in parallel, because they do not edit the same files.

| Lane | Files | Packages, in order |
|------|-------|--------------------|
| API | `crates/api/src/lib.rs`, `tests/fixtures/openapi.json` | WP1, WP5, WP7, WP8b, WP6 |
| Persistence | `crates/persistence/src/catalog.rs`, `crates/persistence/tests/postgres.rs`, new files in `migrations/` | WP3, WP4, WP8b |
| Web | `apps/web/src/lib/api/client.ts`, `apps/web/src/lib/api/types.ts` | WP1, WP5, WP7 |
| Parser | `crates/domain/src/events.rs`, `crates/parsers/src/events.rs`, `tests/fixtures/events/` | WP2 |
| Sports | new crate `crates/sports/` | WP8a |
| Worker | `apps/server/src/main.rs` | WP4 |

Dependency on the v0.2.19 ring fix: the session on `main` edits `crates/api/src/lib.rs` (router, `configuration_control_routes`, and the stream source spec near line 7000 to 7400) and `crates/media/src/session.rs`. No package in this plan edits `default_ring` or `into_byte_stream`. Start the API lane only after v0.2.19 is on `main`. Then rebase on it.

Migration numbers: the last migration is `0074`. The v0.2.19 diff adds no migration. Assign the numbers at merge time, in lane order: WP3 takes `0075` and `0076`, WP4 takes `0077`, WP8b takes `0078` and later. Check `ls migrations` before each merge. Follow the migration rules in `AGENTS.md`.

### 4.2 Execution order

```text
Wave 0: v0.2.19 ring fix (other session, on main)
Wave 1 (parallel): WP1 (API+Web lanes)   WP2 (Parser lane)   WP3 (Persistence lane)   WP8a (Sports lane)   WP10 (read-only)
Wave 2 (parallel): WP4 (Persistence + Worker; needs WP2, WP3)   WP5 (API + Web; needs WP1, WP3)
Wave 3:            WP7 (API + Web; needs WP4, WP5)
Wave 4:            WP8b (API + Persistence; needs WP4, WP7, WP8a)
Wave 5 (parallel): WP6 (API + media/ingest; needs WP8b to clear the API lane)   WP9 (optional; after WP8b)
```

### 4.3 Model guidance

| Package | Model | Reason |
|---------|-------|--------|
| WP1 | Haiku | Mechanical attribute and test changes. The plan lists each struct. |
| WP2 | Sonnet | Regex and parser work with clear fixtures. |
| WP3 | Sonnet | The plan gives the schema and the query change. |
| WP4 | Opus | Data model change, scheduler hook, and data repair. It crosses persistence, worker, and parser. |
| WP5 | Sonnet | One endpoint and one UI control, to a clear specification. |
| WP6 | Opus | The requirements are not clear. Each setting needs a decision: connect it, or remove it. It crosses media, ingest, and API state. |
| WP7 | Sonnet | Handler and UI changes to a clear specification. |
| WP8a | Sonnet | New pure crate with fixtures. Section F7 gives the matching rules. |
| WP8b | Sonnet | Integration to the F7 specification. Escalate to Opus if Jellyfin parses the XMLTV elements differently from the specification. |
| WP9 | Sonnet | Image composition with a known library. |
| WP10 | Sonnet | Read-only investigation. |

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

#### WP4. Event scan engine and scheduling (Opus)

- Findings: F4 B1, B4, B5, B6, and F5 (scan and scheduling).
- Files: `crates/persistence/src/catalog.rs` (`scan_event_channels`, `scan_all_event_channels`, `upsert_event_channel`, `prune_past_event_channels`, `suggest_event_templates`, `build_suggestion_config`), new migration `0077_event_channels_by_channel.sql`, `apps/server/src/main.rs` (`run_provider_reconciliation_finalizer_job`), `crates/persistence/tests/postgres.rs`, `docs/docs/configuration/dynamic-events.md`.
- Depends on: WP2 and WP3.

Brief:

> Rebuild `scan_event_channels` so that it produces event programmes for live provider names. Read section F4 of `docs/audit-plan.md` first.
> (1) Apply `matchRegex` with the Rust `regex` crate, not PostgreSQL `~`. Use a SQL prefilter only on enabled channels: join `provider_streams` to `channel_streams` and `channels` where `channels.enabled`.
> (2) Attach each event to the channel that carries the matched stream. Key `event_channels` by `(template_id, channel_id)`. Add a unique index in the new migration with `CREATE UNIQUE INDEX IF NOT EXISTS`. The table is small, so it needs no `CONCURRENTLY`. Keep `slot_number` for compatibility, but stop the use of the slot regexes.
> (3) Resolve the timezone in this order: template timezone if not `UTC`, then source timezone, then region timezone.
> (4) In the same migration, repair stored templates in an idempotent way: replace `\b` with `\y` in `match_regex`. Change `build_suggestion_config` so that it writes Rust-regex patterns, because step (1) uses the Rust regex engine. Decide and document which regex dialect `matchRegex` uses. Then make the repair agree with that decision.
> (5) Return a scan report with the matched, scheduled, and skipped counts, and the skip reasons. Keep `SaveResult.message` for compatibility.
> (6) Call `scan_all_event_channels` and then prune for each template, with that template's grace, at the end of `run_provider_reconciliation_finalizer_job`. Log errors. Do not fail the job because of a scan error.
> (7) Set `state` from the time: `scheduled` before the start, `live` during the event, `ended` after it, and `hidden` after the grace. Change the output query to include `live`.
> Do not edit `crates/api/src/lib.rs`; WP7 owns the handler changes. Update `docs/docs/configuration/dynamic-events.md` in STE. Done when a PostgreSQL test with the six live titles from WP2 produces event programmes on the correct channels at the correct UTC times, and a second scan run is idempotent.

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

#### WP6. Operator settings wiring (Opus)

- Findings: F9 (except the ring keys) and F11.
- Files: `crates/domain/src/config.rs`, `crates/api/src/lib.rs` (state and startup, `system_info_value`), `crates/media/src/recovery.rs`, `crates/ingest/src/*` (limits), `apps/server/src/main.rs` (if the worker reads ingest limits).
- Depends on: v0.2.19, and on WP8b to clear the API lane.

Brief:

> Section F9 of `docs/audit-plan.md` lists nine operator settings that the runtime never reads. The v0.2.19 release connects the two `media.ring.*` keys. Read how it does this, and use the same pattern. For each other key, decide one action: connect the key to the runtime with its `applyRequirement` semantics (`immediate` or `reimport`), or remove the key from the schema with a clear note. Recommendation: connect `media.viewer.start_delay_seconds`, `media.recovery.window_seconds`, `ingest.max_download_bytes`, and `ingest.max_m3u_line_bytes`. Remove `media.adapter`, or make it the default value for streams without an explicit adapter. Make the template defaults for duration and filler title come from `events.*`. Ask the operator before you remove a key. Also fix F11: compute `providerLimit` from the database sources when no session is active. Done when each kept key has a test that proves a changed override changes the runtime behavior.

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

> Connect `iptv-sports` to the WP4 scan. For each matched event with a known league, call the matcher, and store the result in `generated_programmes.metadata`. Set the title to `<Away full name> at <Home full name>`, and use `categories = ["Sports", "Sports event", <league>]`. An ESPN failure must not stop the scan. Return programme metadata from `list_programmes_for_channels`. In the XMLTV handler, write these elements in DTD order: `<sub-title>` (league and venue; also write the existing `subtitle` for imported programmes), `<desc>`, each `<category>`, `<icon src="...">` (home-team logo), `<episode-num system="original-air-date">YYYY-MM-DD</episode-num>`, and `<live/>`. Before you finish, check how Jellyfin's XMLTV parser reads `icon`, `category`, `episode-num`, and `live`, and record the result in the report. If Jellyfin does not use an element, say so. Done when an XMLTV snapshot test for one NHL event passes, and the output passes `xmllint` against the XMLTV DTD.

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

## 5. Verification after all packages

1. Run `./scripts/run-test-suite.sh` on a host with Docker.
2. Deploy, then run the live checks in each package test plan.
3. Confirm these results against the live instance:
   - `GET /api/v1/event-channels` is not empty after a refresh.
   - The XMLTV output has `Sports` programmes for the enabled NHL, NBA, and NFL event channels at the ESPN start times.
   - A channel rename shows in all three output files.
   - The **Channel aliases**, **Stream health**, **Stream profiles**, **Recordings**, and **Users** tabs show data and accept their forms.
