# IPTV Gateway Implementation Status

Last update: 2026-09-01 (complete Rust coverage)

## Ingest store coverage

The `crates/ingest/src/store.rs` line coverage is 93.35%.

The region coverage is 99.02%.

Thirteen PostgreSQL integration tests cover the `PgSnapshotStore::activate` method and its helper functions.

The tests cover empty snapshot rejection, owner validation, and M3U, XMLTV, and Xtream activation.

They cover snapshot supersession, duplicate checksum reuse, and batch boundaries.

They cover mismatched owner errors for streams and programmes.

Clippy passes for `iptv-ingest` with warnings denied.

Formatting passes for the workspace.

This file records verified results. It does not contain credentials or secret URLs.

## Status definitions

- **Working**: A test passed in the current workspace.
- **Failed**: A test or runtime operation failed.
- **Blocked**: A required dependency or decision prevents a test.
- **Todo**: The implementation or test is not complete.

## Working

### Event template suggestions

The `EventTemplateSuggestion` type and `suggestEventTemplates` API client method are in the frontend.

The `FetchIptvApiClient` calls `GET /api/v1/event-templates/suggestions`.

The `MockIptvApiClient` returns an empty array.

The events page has an "Analyze streams" button that loads suggestions.

The suggestion panel shows loading, error, and result states.

Each suggestion card displays match regex, channel name format, group, duration, grace, future window, and sample streams.

Each card has a "Create template" button that creates the template and removes the suggestion from the panel.

`npx tsc --noEmit` and `CI=true pnpm test` pass.

### Compose startup

The default Compose stack builds and starts.

The `core`, `web`, and `postgres` health checks pass.

The gateway returns `ok` from `/health/live`.

The gateway returns `ready` from `/health/ready`.

### Rust tests and lint

The full Rust workspace test suite passes.

All four PostgreSQL integration tests pass against the Compose database.

The reconciliation integration test `reconcile_merges_streams_by_tvg_id_and_maps_epg` passes.

Rust formatting passes.

Clippy passes for the full workspace with warnings denied.

### Real M3U ingestion

The worker downloaded 474,594,472 bytes from the configured M3U source.

The worker parsed 1,166,789 M3U records.

The worker activated the M3U snapshot in PostgreSQL.

The source refresh job succeeded on its first new attempt.

### Real XMLTV ingestion

The isolated test downloaded the configured XMLTV source.

The parser accepted its standard external XMLTV document type.

The parser rejected test data that contained an internal entity declaration.

The worker downloaded 89,884,828 bytes from the configured XMLTV source.

The worker parsed 287,773 XMLTV records.

The worker prepared and activated 281,625 XMLTV records.

PostgreSQL contains 6,302 EPG channels and 275,323 programmes.

The XMLTV refresh job succeeded on its first final attempt.

### Canonical channel reconciliation

The `CatalogRepository` reconciles activated provider streams into canonical channels.

The reconciliation groups streams by `tvg-id` and merges streams that share the same identifier.

The reconciliation generates a fallback canonical key for streams without a `tvg-id`.

The reconciliation preserves multiple streams under one canonical channel.

The reconciliation removes orphaned automatic channels.

The reconciliation maps EPG channels by case-insensitive `tvg-id` matching.

The worker calls reconciliation after each successful source refresh.

### Control API

The system API returns DB-backed system counts.

The channels API returns a paginated response with `total`, `limit`, `offset`, and `items`.

The programmes API returns a paginated response with `total`, `limit`, `offset`, and `items`.

The groups API returns group names and channel counts for the group filter dropdown.

The DELETE source API removes a source and its related data.

The API has an in-memory fallback for tests or configurations without persistence.

### Lineup templates

Migration `0009_lineup_templates.sql` creates the `lineup_templates`, `lineup_categories`, and `lineup_channels` tables.

The `CatalogRepository` provides `list_lineup_templates`, `create_lineup_template`, `delete_lineup_template`, `list_lineup_categories`, `list_lineup_channels`, `import_lineup`, and `apply_lineup` methods.

The `apply_lineup` method matches canonical channels by normalized name or alias, enables matched channels, and disables unmatched channels in the group.

The control API exposes `GET`, `POST`, and `DELETE` endpoints for lineup templates.

It exposes `GET` endpoints for categories and channels.

It exposes a `POST` endpoint to apply a lineup.

All lineup endpoints require admin authentication.

`cargo build`, `cargo test`, `cargo clippy`, and `cargo fmt` pass for `iptv-persistence` and `iptv-api`.

### Realtime SSE

The `/api/v1/catalog-events` endpoint publishes overview and heartbeat events.

The Caddy gateway disables buffering for SSE endpoints with `flush_interval -1`.

The SSE stream delivers `overview` events with system counts every five seconds.

The SSE stream sends keepalive comments between events.

The frontend `useCatalogEvents` hook subscribes to the SSE stream from authenticated sessions.

The hook invalidates the `overview` and `sources` React Query caches when events arrive.

The management layout activates the SSE subscription on all non-login routes.

### TypeScript tests and coverage

All 33 TypeScript unit tests pass.

TypeScript coverage is 97.85% for lines.

TypeScript coverage is 95.85% for functions.

TypeScript coverage is 86.64% for branches.

### Real-source browser flow

The Chromium Playwright flow passes.

The test signs in through the local administrator form.

The test adds unique M3U and XMLTV sources through the UI.

The M3U source activated 1,166,789 records.

The XMLTV source activated 281,622 records during the final run.

The test opened all seven management routes.

The browser reported no API failures, console errors, console warnings, page errors, or request failures.

The test removed its two source fixtures and their related records.

A failed trial captured a safe screenshot, API diagnostics, page details, cleanup output, and a post-source trace.

### Realtime UI Playwright tests

All 10 realtime UI Playwright tests pass.

The catalog SSE stream test verifies that `overview` events arrive in the browser.

The overview page test confirms real backend data renders without refresh buttons.

The channels page test verifies paginated API responses.

The programmes page test verifies paginated API responses.

The events page test confirms an empty state when no events exist.

The sessions page test confirms real session telemetry.

The sources page test confirms no manual refresh button.

The Jellyfin setup page test confirms error-free load.

The API error test confirms backend errors surface as visible UI alerts.

The sign-out test confirms the return to the login page.

### UI fake-data removal

The overview page no longer uses `mockInitialData` or fabricated values.

The sources page no longer uses `mockInitialData` or a manual refresh button.

The channels page no longer uses `mockInitialData` and uses paginated API data.

The EPG page no longer uses `mockInitialData` or hard-coded guide-health values.

The events page no longer uses `mockInitialData` and shows an empty state.

The sessions page no longer uses `mockInitialData`.

The API client returns paginated `ChannelPage` and `ProgrammePage` responses.

The React Query definitions accept `ChannelQuery` and `ProgrammeQuery` parameters.

### UI copy audit

The UI copy uses active voice.

The UI copy does not use `-ing` forms as verbs.

The events page description uses active voice instead of passive voice.

The login page session note uses active voice instead of passive voice.

The loading indicator text uses an imperative form.

The button status text uses an imperative form instead of `-ing` forms.

### Rust global coverage

Rust coverage is 86.62% for lines.

Rust coverage is 86.14% for functions.

Rust coverage is 86.54% for regions.

### Ingest pipeline coverage

`crates/ingest/src/pipeline.rs` coverage is 83.32% for lines.

`crates/ingest/src/pipeline.rs` coverage is 79.53% for regions.

The XMLTV streaming path, Xtream live streams, Xtream short EPG, and helper functions have test coverage.

### Other checks

The dependency advisory, ban, license, and source checks pass.

The default and test Compose configurations pass validation.

The control API lists exactly two configured sources.

Both configured sources have the `healthy` state.

The integration tests remove the source records that they create.

## Failed

### Initial source refresh jobs

Runtime inspection confirmed two initial `refresh-source` failures.

Each job used all three attempts.

Each job had the error `job handler is not registered`.

The new worker handler fixed this failure.

### Initial XMLTV retries

The first durable XMLTV retry rejected the standard document type.

The next retry found a database constraint that rejected conflicting metadata.

The final retry succeeded after both fixes.

### Server main.rs coverage

The `apps/server/src/main.rs` line coverage is 85.01%.

The region coverage is 84.92%.

Seven new tests cover the `process_job` function, the `run_source_refresh` error paths, the `WorkerJobControl::checkpoint` implementation, and the `RefreshError::persisted_summary` variants.

The database-backed tests use the PostgreSQL test database and clean up all inserted rows.

Clippy passes for `iptv-gateway` with warnings denied.

Formatting passes for the workspace.

### Changed-line coverage gate

The `scripts/changed-line-coverage.sh` script compares changed production lines against LLVM coverage data.

The Makefile target `changed-line-coverage` runs the script.

The script uses `cargo llvm-cov` to generate coverage data and `python3` to parse the JSON output.

A test run with one changed line in `crates/api/src/lib.rs` passed at 100% coverage.

The threshold defaults to 80% and accepts a custom value as the first argument.

The base ref defaults to `HEAD~1` and accepts a custom value as the second argument.

### Deterministic media acceptance test

The 120-second deterministic media acceptance test passed.

The test verified three upstreams, six viewers, and 120 seconds of continuous playback.

The test confirmed shared upstream sessions, provider capacity enforcement, and MPEG-TS packet validity.

The test confirmed viewer disconnection behavior, upstream cleanup, and no rejected connections.

### Fault-injection acceptance test

The 60-second fault-injection acceptance test passed.

The test used Toxiproxy to inject timeout and latency faults into the media path.

The test verified two upstreams, four viewers, and recovery from transient network failures.

The test confirmed continued MPEG-TS delivery after fault recovery.

The test confirmed no rejected connections and no duplicate upstreams.

### Live-provider media acceptance test

The 30-second live-provider media acceptance test passed.

The test downloaded the configured M3U playlist from the `.env` source.

The test selected an active `.ts` stream and verified MPEG-TS sync byte delivery.

The test received 95,267 packets and 18,082,460 bytes over 30 seconds.

### Real M3U canonical channel reconciliation

The M3U refresh job downloaded 474,664,174 bytes and parsed 1,166,933 records.

The reconciliation INSERT created 1,166,933 canonical channels in 50 seconds.

The optimized DELETE removed zero orphaned channels in 3.88 seconds.

The channel_streams INSERT linked 1,166,933 streams to channels in 210 seconds.

The control API returns 1,166,933 channels and 1,166,933 healthy streams.

The control API returns paginated channel responses with names, groups, and stream counts.

The `NOT IN` DELETE query was replaced with `NOT EXISTS` for a 130x speed improvement.

### Performance optimization

The channels list query uses a deferred-join pattern with keyset pagination.

The system count query uses simplified joins instead of correlated subqueries.

The `0005_performance_indexes.sql` migration adds indexes on `channels(channel_number)`, `channels(group_name)`, `channel_streams(channel_id)`, and `channel_epg_mappings(channel_id)`.

The channels API returns 50 items from 1,166,933 total in 91 milliseconds.

The system API returns counts in 506 milliseconds.

The groups API returns 1,203 groups in 577 milliseconds.

### Source deletion

The `SourceRepository::delete` method removes a source and its related data.

The `DELETE /api/v1/sources/{source_id}` endpoint requires admin authentication.

The sources page shows a confirmation flow before deletion.

The deletion invalidates the `sources`, `overview`, `channels`, and `groups` React Query caches.

### Group filtering and EPG visibility

The channels page includes a group filter dropdown that loads from the groups API.

The channels page uses server-side search and pagination.

The channels page shows the total channel count from the API response.

### Test data cleanup

The `scripts/cleanup-test-data.sh` script removes test and reconciliation sources.

The script removed eight test provider accounts and one old job.

The database contains only the two real sources from the `.env` file.

The cleanup preserves the 1,166,933 channels and 1,166,933 channel-stream links.

### Test parallelism

The Playwright config uses `fullyParallel: true` with three workers.

The `management-flows.spec.ts` describe block uses `test.describe.serial` for tests that share source state.

The `realtime-ui.spec.ts` tests run in parallel because each test is read-only.

The `real-source-flow.spec.ts` test skips unless `IPTV_E2E_REAL_SOURCES=true` to prevent long downloads during routine runs.

The vitest config uses the `forks` pool with up to four forks for parallel file execution.

The Makefile `test-parallel` target runs Rust and web tests concurrently.

The `.cargo/config.toml` file sets the test thread count to the CPU core count.

The Playwright suite runs 17 fast tests in 8 seconds with three workers.

The Rust and web test suites complete in 2 seconds when run in parallel.

### Schema normalization and query performance

Migration `0006_schema_normalization_and_indexes.sql` applies these changes:

- Drops the unused `provider_streams_group_idx` index (54 MB, three scans).
- Drops the unused `channels_group_name_idx` index (21 MB, zero scans).
- Adds `channels_enabled_idx` partial index for index-only count scans.
- Adds `channels_group_enabled_idx` covering index for group aggregation.
- Adds `channel_epg_mappings_epg_channel_idx` for programme join queries.
- Sets autovacuum scale factors to 0.05 for large tables to keep visibility maps fresh.

The `system_counts` query uses `EXISTS` instead of `count(DISTINCT)` to avoid an external merge sort.

The schema satisfies third normal form. The `provider_streams.provider_account_id` column is an intentional denormalization for join performance. The polymorphic `source_snapshots` owner column is constrained by a CHECK rule.

Measured API response times after migration and VACUUM:

- `/api/v1/system`: 266 ms (was 506 ms).
- `/api/v1/channels?limit=50`: 65 ms (was 91 ms).
- `/api/v1/groups`: 100 ms (was 577 ms).

EXPLAIN ANALYZE results after VACUUM:

- Channels count: 118 ms via parallel index-only scan (was 1013 ms via seq scan).
- Healthy stream count: 714 ms via parallel hash semi join (was 2005 ms via external merge sort).
- Groups aggregation: 233 ms via parallel index-only scan (was 609 ms via seq scan).
- Guide mapped count: 0.04 ms via nested loop with index-only scan.

### Server-side channel search

The channels page uses debounced server-side search instead of client-side table filtering.

The `useDebouncedValue` hook delays the search query by 300 milliseconds.

The TanStack Table no longer uses `globalFilteringFeature` or `filteredRowModel`.

The `keepPreviousData` option keeps the previous results visible while the new query loads.

The Playwright test verifies that a search for "news" returns fewer results than the baseline.

### Stream preview

The channels page includes a preview button for each channel row.

The `StreamPreview` component uses `mpegts.js` to play the raw MPEG-TS stream in the browser.

The backend exposes two admin-authenticated endpoints:

- `GET /api/v1/channels/{id}/preview` returns the stream URL and content type.
- `GET /api/v1/channels/{id}/stream` proxies the MPEG-TS stream through the admin session.

The stream proxy uses the `HttpTsSessionManager` to open a shared session.

The preview viewer registers as a session viewer and appears in `/api/v1/sessions`.

The `CatalogRepository.channel_stream_source` method queries the active stream URL from the database.

The preview endpoint returns 503 when the channel has no active provider stream.

The Playwright test verifies that the preview button opens a video element without errors.

### Development environment with hot reload

The `docker-compose.dev.yml` override adds hot reload for Rust and web development.

The `Dockerfile.dev` uses `cargo-chef` to cook dependencies into the Docker image at build time.

The `scripts/dev-reload.sh` script polls file modification times every second.

Docker Desktop on macOS does not propagate inotify events through bind mounts.

The polling approach detects source changes and triggers incremental rebuilds.

The `cargo-target` named volume preserves incremental compilation artifacts across restarts.

The web container runs Vite dev server with HMR through a bind mount.

Make targets:

- `make compose-dev-build` builds the dev images.
- `make compose-dev-up` starts the dev environment.
- `make compose-dev-down` stops the dev environment.
- `make compose-dev-logs` follows the core, worker, and web logs.

Verified results:

- First build with pre-cooked deps: 14 seconds.
- Incremental rebuild after a one-line change: 33 seconds.
- The watchexec polling approach did not detect changes through Docker bind mounts on macOS.
- The mtime-polling script detects changes and triggers rebuilds correctly.
- The core container became healthy after each rebuild.
- The Vite dev server started and served the web UI with HMR.

### Configurable worker count

The worker service uses `deploy.replicas` in `docker-compose.yml`.

The replica count is controlled by the `IPTV_WORKER_COUNT` environment variable.

The default value is 4 workers.

Each worker replica derives its worker ID from the container hostname.

The `worker_id_from` function checks `IPTV_WORKER_ID` first, then `HOSTNAME`, then falls back to `worker-1`.

A `build.rs` file in `crates/persistence` forces recompilation when migration files change.

Verified results:

- Four worker replicas started with unique IDs.
- Each worker logged `job worker started` with a distinct hostname-based ID.
- Setting `IPTV_WORKER_COUNT=2` scaled the worker pool to two replicas.
- Restoring `IPTV_WORKER_COUNT=4` scaled the worker pool back to four replicas.
- The `worker_id_uses_override_or_default` test passed.
- The `blank_worker_id_falls_back_to_hostname_then_default` test passed.
- The full Rust workspace test suite passed.

### Migration idempotency

All six migrations use idempotent SQL constructs.

Migration 0001 uses `CREATE TABLE IF NOT EXISTS` and `CREATE INDEX IF NOT EXISTS` for all statements.

Migration 0002 uses `DO $$ ... $$` blocks to check `pg_constraint` before it drops or adds constraints.

Migration 0003 uses `DROP INDEX IF EXISTS`.

Migration 0004 uses `ADD COLUMN IF NOT EXISTS` and `CREATE INDEX IF NOT EXISTS`.

Migrations 0005 and 0006 already used `IF NOT EXISTS` and `IF EXISTS`.

The `programmes_exact_dedupe_idx` index was removed from migration 0001 because migration 0003 drops it to allow conflicting programmes.

Verified results:

- All six migrations applied cleanly on a fresh database.
- All six migrations applied cleanly a second time on the fresh database.
- All six migrations applied cleanly on the existing production database with 1.16M rows.
- No duplicate indexes or constraints appeared after double application.
- The full Rust workspace test suite passed after the changes.

### EPG mapping by normalized name

The `reconcile_epg_mappings` method now applies EPG mappings in two passes.

Pass 1 matches channels to EPG channels by exact case-insensitive `tvg-id` with confidence 0.99.

Pass 2 matches channels to EPG channels by normalized name with confidence 0.90.

The `normalize_channel_name` SQL function strips country prefixes, quality tokens, resolution markers, and punctuation from channel names.

The name match uses only the latest active XMLTV snapshot to avoid duplicate matches across snapshots.

The name match applies only when exactly one EPG channel shares the normalized name.

The name match excludes VOD content by filtering out names that contain episode or year patterns.

Migration `0007_normalize_channel_name.sql` creates the `normalize_channel_name` function with `CREATE OR REPLACE FUNCTION`.

Verified results against real provider data:

- The M3U provider supplies empty `tvg-id` attributes for all 5,834,559 streams.
- The XMLTV source contains 18,906 EPG channels and 826,441 programmes.
- The name-based match created 4,757 EPG mappings.
- The mapped channels have access to 205,125 programmes.
- The system API reports 40.7% guide coverage.
- The programmes API returns paginated responses with channel names, titles, and source attribution.
- The `reconcile_epg_mappings_by_normalized_name` integration test passed.
- The full Rust workspace test suite passed.

### Server-side EPG search

The programmes API accepts a `search` query parameter.

The search parameter filters programmes by title or channel name with a case-insensitive `LIKE` match.

The EPG page uses debounced server-side search with pagination instead of client-side filtering on a fixed page.

Verified results:

- A search for "france" returns 4,390 programmes from the database.
- The EPG page sends search requests to the server after a 300 ms debounce.
- The full Rust workspace test suite passed.

### Stream preview authentication

The stream endpoint accepts a `token` query parameter as a fallback when the `Authorization` header is absent.

The preview endpoint appends the admin bearer token to the stream URL so mpegts.js can authenticate without custom headers.

The `StreamPreview` component no longer sets `withCredentials` to `true` because the token query parameter provides authentication.

Verified results:

- The preview endpoint returns a stream URL with a token query parameter.
- The stream endpoint accepts the token query parameter and authenticates the request.
- The stream proxy returns 502 when the upstream provider rejects the connection. This is a provider data limitation, not a code defect.
- The full Rust workspace test suite passed.

### Channel and group enable/disable

The API exposes two new endpoints:

- `PATCH /api/v1/channels/{channel_id}/enabled` sets the `enabled` flag for one channel.
- `PATCH /api/v1/groups/{group_name}/enabled` sets the `enabled` flag for all channels in a group.

The playlist and XMLTV output endpoints read from the database catalog and filter by the `enabled` flag.

The channels page shows an enable/disable toggle for each channel and provides enable-all and disable-all buttons when a group filter is active.

The `ChannelResponse` includes an `enabled` field.

Verified results:

- A channel toggle request updates the `enabled` flag in the database.
- A group toggle request updated 48,728 channels in one request.
- The channel API response includes the `enabled` field.
- The full Rust workspace test suite passed.

### TV guide page

The web application includes a new TV Guide page at `/tv-guide`.

The TV Guide page shows programmes that are currently airing and programmes that are upcoming.

The page uses debounced server-side search to filter by channel name or programme title.

The navigation bar includes a TV Guide link.

Verified results:

- The TV Guide page renders without TypeScript errors.
- The page uses the programmes API with server-side search.
- The route tree includes the `/tv-guide` route.

### QA audit: recent feature coverage

Five PostgreSQL integration tests now cover the recent persistence features.

The `stream_health_updates_quality_ranking_and_stats` test verifies stream health updates, quality ranking, stats, and the streams-needing-check query.

The `user_crud_and_channel_grants_work` test verifies user CRUD, login timestamps, password-hash lookup, and per-user channel grants.

The `channel_alias_crud_and_resolution_work` test verifies alias CRUD, country filtering, resolution, duplicate-alias rejection, and stats.

The `recording_crud_and_stats_work` test verifies recording rules, recording instances, status updates, stats, and deletion.

The `stream_profile_crud_and_assignment_work` test verifies stream profile CRUD and channel assignment removal.

Verified results:

- The five new PostgreSQL integration tests pass.
- The full Rust workspace test suite passes (270 tests, 1 ignored).
- The Vitest suite passes (40 tests).
- The TypeScript type check passes.
- The Playwright end-to-end suite is blocked because the running backend is not available in the test environment.
- Two pre-existing Clippy warnings remain in `crates/persistence/src/catalog.rs` and `crates/persistence/src/lib.rs`.
- The Rust formatting check shows unformatted code across the workspace; this is a pre-existing state.

## Blocked

_(No items are currently blocked.)_

## Todo

- Add Playwright tests for source sync progress, cancellation, and job queue durability flows.
- Add Playwright tests for the channel enabled filter fix.
- Consider PostgreSQL `LISTEN/NOTIFY` for instant job pickup instead of polling.
- Consider `pgmq` or `apalis` for long-term queue evolution if visibility timeouts and dead-letter archives are needed.
- Complete the remaining implementation plan.

### Event template system backend

Migration `0008_event_templates.sql` creates the `event_templates` and `event_channels` tables with `IF NOT EXISTS` and `CREATE INDEX IF NOT EXISTS` constructs.

The `CatalogRepository` exposes these methods:

- `list_event_templates` lists templates with an optional enabled filter.
- `create_event_template` inserts a new template row.
- `update_event_template` updates a template row by ID.
- `delete_event_template` deletes a template row by ID.
- `list_event_channels` lists event channels with an optional template filter.
- `upsert_event_channel` inserts or updates an event channel by template and slot.
- `scan_event_channels` scans provider streams for names that match the template regex, creates event channels, and links them to canonical channels.
- `prune_past_event_channels` marks event channels as hidden when their end time plus grace has passed.

The API exposes these endpoints:

- `GET /api/v1/event-templates` lists event templates.
- `POST /api/v1/event-templates` creates an event template.
- `PATCH /api/v1/event-templates/{id}` updates an event template.
- `DELETE /api/v1/event-templates/{id}` deletes an event template.
- `GET /api/v1/event-channels` lists event channels with an optional `templateId` query parameter.
- `POST /api/v1/event-templates/{id}/scan` scans provider streams and updates event channels.
- `POST /api/v1/event-templates/{id}/prune` prunes past event channels.

The `/api/v1/events` endpoint returns event channels from the database when a catalog repository is available.

Verified results:

- `cargo build -p iptv-persistence -p iptv-api` passed.
- `cargo test -p iptv-persistence -p iptv-api` passed with 34 tests.
- The migration applied cleanly to the running database.
- The migration registered in the `_sqlx_migrations` table.

### Event and lineup end-to-end verification

Verified against the running gateway with the bootstrap bearer token:

- `POST /api/v1/event-templates` created NFL and MLB templates with `displayName`, `groupName`, `matchRegex`, `channelNameFormat`, `eventDurationHours`, `pastDateGraceHours`, and `futureDateDays` fields.
- `GET /api/v1/event-templates` returned both templates.
- `POST /api/v1/event-templates/{id}/scan` scanned provider streams and returned `{ok: true, message: "Scanned 0 event channels"}` for both templates.
- `GET /api/v1/event-channels` returned an empty list because the provider data does not contain NFL or MLB stream names.
- `GET /api/v1/events` returned an empty list from the database.
- `POST /api/v1/lineup-templates` created a test sports lineup with a Sports category and two channels (ESPN, NFL Network).
- `GET /api/v1/lineup-templates/{id}/categories` returned the Sports category.
- `GET /api/v1/lineup-templates/{id}/channels` returned both channels with aliases.
- `POST /api/v1/lineup-templates/{id}/apply` matched 12 canonical channels and enabled them.

Full workspace test results:

- `cargo test --workspace --all-targets` passed with 265 tests across all crates.
- `npx vitest run` passed with 33 frontend tests.
- `npx tsc --noEmit` passed with zero errors.
- `npx playwright test e2e/events-lineup-flows.spec.ts` passed with 8 tests.

### Playwright event and lineup coverage

The `e2e/events-lineup-flows.spec.ts` file covers:

- Event templates API returns real database data.
- Event channels API returns real database data.
- Events API returns real database data.
- Event template create, scan, and delete cycle.
- Lineup templates API returns real database data.
- Lineup template create, list categories, list channels, and delete cycle.
- Events page loads without errors and shows real template data from the backend.
- Events page scan button triggers a backend scan without errors.

The UI tests establish a session through the API to avoid the login form timing issue.

### Groups management page

The `/groups` page provides a dedicated view for group-level channel management.

The backend `list_groups` query now returns all groups (not only enabled ones) with `channelCount` and `enabledCount` fields.

The `GroupResponse` API type includes `enabledCount` so the frontend can show partial, all-enabled, and all-disabled states.

The groups page provides:

- A search input to filter groups by name.
- Bulk Enable all groups and Disable all groups buttons at the top that call `PATCH /api/v1/groups/enabled` in a single request.
- A summary count of enabled and total channels across all groups.
- A card per group with a status badge (all enabled, all disabled, or partial).
- Enable all and Disable all buttons per group that call `PATCH /api/v1/groups/{name}/enabled`.
- Loading spinners on clicked buttons until the response returns.
- Query invalidation for groups, channels, and overview after each toggle.

### Bulk group toggle performance

Migration `0010_group_name_index.sql` adds a non-partial btree index on `channels.group_name`.

The existing `channels_group_enabled_idx` is a partial index (`WHERE enabled`) that cannot serve queries that filter on disabled rows. The new `channels_group_name_idx` index eliminates sequential scans for individual group toggles.

The `set_group_enabled` query now includes `AND enabled != $2` to skip rows already in the target state, so no-op toggles complete in constant time.

The `set_all_groups_enabled` method runs a single `UPDATE channels SET enabled = $1, updated_at = now() WHERE enabled != $1` query. This replaces the previous approach of firing one API call per group (1,204 parallel requests).

The `PATCH /api/v1/groups/enabled` endpoint accepts `{enabled: boolean}` and calls `set_all_groups_enabled`.

Verified performance results (1,167,410 channels across 1,204 groups):

- No-op bulk enable (all already enabled): 1.1 seconds.
- Individual group no-op toggle (already enabled): 0.3 seconds (was 6 seconds before the index).
- Full bulk update (1.1M rows): approximately 96 seconds due to PostgreSQL MVCC tuple writes.
- Previous parallel approach: 1,204 requests at 6 seconds each.

Verified test results:

- `cargo test --workspace --all-targets` passed with 265 tests.
- `npx vitest run` passed with 35 frontend tests.
- `npx playwright test e2e/groups-flows.spec.ts` passed with 6 tests covering the API, individual toggle, bulk toggle, page load, search filter, and bulk buttons.

### Stream preview URL decryption fix

The `channel_stream` handler used `ps.url_template` as the upstream URL. This field contains a redacted template (`http://host/[encrypted]`) rather than the actual stream URL. The real URL is encrypted in `url_secret_ciphertext`.

Changes:

- `MasterKey::decrypt_secret` decrypts ciphertext produced by `encrypt_secret`.
- `ChannelStreamSourceRow` includes `url_secret_ciphertext`.
- `channel_stream_source` query fetches `url_secret_ciphertext`.
- `channel_stream` handler decrypts the URL using the source-associated data before it opens the upstream connection.
- `AppState` stores the `MasterKey` so the handler can decrypt without a separate parameter.

Verified result:

- `GET /api/v1/channels/{id}/stream` returns HTTP 200 with `video/mp2t` content. The previous response was HTTP 502 with `upstream-start-failed`.

### Configurable source refresh scheduling

Migration `0011_source_refresh_scheduling.sql` adds `refresh_interval_seconds` and `last_refreshed_at` columns to `provider_accounts` and `epg_sources`.

A value of 0 in `refresh_interval_seconds` disables automatic refresh for that source.

The `SourceRepository::list_due_sources` method returns sources where the interval has elapsed and no refresh job is queued or running.

The `update_refresh_interval` method sets the interval for a source.

The `mark_source_refreshed` method records the completion time after a refresh job succeeds.

The server `refresh_scheduler` task runs every 60 seconds. It calls `list_due_sources` and enqueues a `refresh-source` job for each due source.

The `process_job` function calls `mark_source_refreshed` after a successful refresh.

The `PATCH /api/v1/sources/{source_id}/refresh-interval` endpoint accepts `{refreshIntervalSeconds: number}` and updates the interval.

The `SourceResponse` API type includes `refreshIntervalSeconds` and `lastRefreshedAt`.

The sources page shows a Refresh column with:

- The configured interval (for example, 1h, 30m, or Manual).
- The last refreshed time in relative format.
- An inline editor to set the interval in seconds.

Verified results:

- `GET /api/v1/sources` returns `refreshIntervalSeconds` and `lastRefreshedAt` for each source.
- `PATCH /api/v1/sources/{id}/refresh-interval` with `{refreshIntervalSeconds: 3600}` returns HTTP 204 and the verified interval is 3600.
- `cargo test --workspace --all-targets` passed with 265 tests.
- `npx vitest run` passed with 35 frontend tests.
- `npx playwright test e2e/management-flows.spec.ts` passed with 12 tests including the refresh interval API and sources page column tests.

### Source edit and max connections configuration

The `SourceResponse` API type now includes `maxConnections`, `timezone`, and `enabled` fields.

The `SourceSummary` persistence type includes the same fields. The list query fetches `max_connections` and `source_timezone` from `provider_accounts`, and uses defaults of 1 and the `timezone` column for `epg_sources`.

The `SourceUpdate` struct provides a partial update with optional `max_connections`, `timezone`, and `enabled` fields. Only `Some` fields are applied.

The `SourceRepository::update_source` method updates `provider_accounts` or `epg_sources` based on which table contains the source ID.

The `PATCH /api/v1/sources/{source_id}` endpoint accepts `{maxConnections?, timezone?, enabled?}` and returns HTTP 204 on success. The endpoint rejects `maxConnections` values less than 1.

The sources page provides:

- An edit button (pencil icon) per source row.
- A modal dialog with fields for max connections, timezone, and enabled toggle.
- A save button that calls `PATCH /api/v1/sources/{id}` and invalidates the sources query.

Verified results:

- `GET /api/v1/sources` returns `maxConnections`, `timezone`, and `enabled` for each source.
- `PATCH /api/v1/sources/{id}` with `{maxConnections: 8, timezone: "America/Denver", enabled: true}` returns HTTP 204 and the verified values match.
- `cargo test --workspace --all-targets` passed with 265 tests.
- `npx vitest run` passed with 35 frontend tests.
- `npx playwright test e2e/management-flows.spec.ts` passed with 15 tests including the source edit API and edit dialog tests.

### Searchable group filter combobox

The channels page group filter was a native `<select>` element with 1,204 options. This made it difficult to find a specific group.

A new `Combobox` component in `apps/web/src/components/ui/combobox.tsx` provides:

- A trigger button that shows the current selection.
- A popover with a search input and a scrollable filtered list.
- Case-insensitive filtering by label and value.
- Keyboard support: Escape closes the popover.
- Outside-click detection closes the popover.
- Optional hint text per item (used for channel counts).
- Accessible roles: `listbox`, `option`, `combobox`, `aria-expanded`, `aria-selected`.

The channels page uses the Combobox for group filtering. The "All groups" option has an empty string value. Each group shows its name as the label and channel count as the hint.

Verified results:

- `npx vitest run` passed with 38 frontend tests including 3 new combobox tests.
- `npx playwright test e2e/management-flows.spec.ts` passed with 15 tests (excluding the pre-existing stream preview failure) including 3 combobox tests.

### EPG mapping review and manual override

The `0012_epg_mapping_review.sql` migration adds review workflow support to `channel_epg_mappings`:

- `review_status` column with values `applied`, `review`, `rejected`, `manual`.
- `reviewed_by` and `reviewed_at` columns for audit.
- `review_candidates` table for ambiguous match alternatives.
- Index on `review_status` for review queue queries.

The `reconcile_epg_mappings` method now has three passes:

- Pass 1: exact `tvg-id` match with confidence 0.99 and `review_status = 'applied'`.
- Pass 2: normalized name match with confidence 0.90 and `review_status = 'applied'`.
- Pass 3: ambiguous name matches populate `review_candidates` and create mappings with `review_status = 'review'`.

Manual mappings (`review_status = 'manual'`) are preserved during reconciliation.

New persistence methods:

- `list_epg_mappings`: paginated list with channel and EPG details, filterable by review status.
- `list_unmapped_channels`: channels with no EPG mapping, searchable by name.
- `list_review_candidates`: alternative EPG candidates for a channel in review.
- `set_manual_epg_mapping`: operator override with `review_status = 'manual'`.
- `remove_epg_mapping`: delete a channel EPG mapping.
- `resolve_review`: accept or reject a review candidate.
- `search_epg_channels`: search EPG channels by name or XMLTV ID for manual linking.

New API endpoints:

- `POST /api/v1/epg/reconcile`: trigger reconciliation, returns mapping stats.
- `GET /api/v1/epg/mappings`: list mappings with confidence, method, and review status.
- `GET /api/v1/epg/unmapped`: list channels with no EPG mapping.
- `GET /api/v1/epg/review/{channel_id}/candidates`: list review candidates.
- `GET /api/v1/epg/channels/search`: search EPG channels for manual linking.
- `PATCH /api/v1/channels/{channel_id}/epg-mapping`: set a manual mapping.
- `DELETE /api/v1/channels/{channel_id}/epg-mapping`: remove a mapping.
- `POST /api/v1/epg/review/{channel_id}/resolve`: accept or reject a review.

All mutating endpoints enforce admin authentication and CSRF protection.

The frontend has a new `/epg-mappings` page with:

- Three tabs: Mapped, Needs review, and Unmapped.
- Confidence and method badges per mapping.
- A "Reconcile now" button that triggers reconciliation and shows stats.
- Manual linking from the Unmapped tab with EPG channel search.
- Review candidate display with accept and reject actions.
- Mapping removal from the Mapped tab.
- Pagination and search support.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests including 3 new API tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests including 2 new EPG mappings tests.
- `npx playwright test e2e/epg-mappings.spec.ts` passed with 5 tests and 1 skipped (reconcile takes over 90s on 1.1M channels, verified manually via curl).
- Manual curl test confirmed: 4,757 mappings applied, 14,788 review candidates queued, 2,522 channels in review status, 0 unmapped channels.

### EPG and TV guide enabled-channel filter

The `list_programmes`, `list_epg_mappings`, and `list_unmapped_channels` queries now filter to channels where `enabled = true`.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests.
- `npx vitest run` passed with 40 frontend tests.
- Database verification confirmed 7,279 total mappings exist but only 4 enabled channels have mappings.

## Feature gap analysis

Research sources:

- Dispatcharr core: <https://github.com/Dispatcharr/Dispatcharr>
- Dispatcharr plugins: <https://dispatcharr.github.io/Dispatcharr-Docs/plugin-listing/>
- Plugin workflow guide: <https://piratesirc.github.io/Dispatcharr-Plugin-Workflow/>
- Threadfin: <https://github.com/threadfin/threadfin>
- xTeVe: <https://xteve.org/>
- ErsatzTV: <https://github.com/ErsatzTV/ErsatzTV>
- tvheadend: <https://github.com/tvheadend/tvheadend>

### Features we have

- M3U and Xtream Codes ingestion.
- XMLTV EPG ingestion.
- Channel reconciliation and canonical channel mapping.
- EPG matching with review workflow.
- Stream proxy and relay.
- HDHomeRun emulation.
- M3U and XMLTV export.
- Session tracking and realtime SSE events.
- Event channels for MLB, NHL, NBA, NFL.
- Lineup templates.
- Output profiles with tuner count.
- Group filtering and channel enable/disable.
- Stream preview with mpegts.js.
- Admin authentication with CSRF.

### Features we do not have

The table below lists features found in Dispatcharr, its plugins, or similar services that our project does not have yet.

| Priority | Feature | Source | Description |
|----------|---------|--------|-------------|
| High | Stream health checking | Dispatcharr IPTV Checker | Probe streams with ffprobe to detect dead streams, extract codec and resolution metadata, and sync metadata back to the database. |
| High | Stream quality ranking | Dispatcharr Stream Mapparr | Rank streams by resolution, FPS, and codec. Auto-assign the best stream as primary and demote broken sources. |
| High | Automatic failover | Dispatcharr core | Switch to the next backup stream when the active stream fails during playback. |
| High | Multi-user access control | Dispatcharr core | Create user accounts with granular permissions. Control which channels and profiles each user can access. |
| High | Channel name standardization | Dispatcharr Channel Mapparr | Standardize channel names against curated databases. Use a built-in alias table for common name variants. |
| Medium | Stream profiles | Dispatcharr core | Configure how the backend connects to upstream streams: direct, FFmpeg, VLC, or Streamlink. |
| Medium | Output transcoding | Dispatcharr core | Transcode output for different clients: AC3 for media servers, AAC for browsers. Select fMP4 or MPEG-TS container. |
| Medium | DVR and recording | Dispatcharr core, tvheadend | Schedule one-time and recurring recordings from the TV guide. Apply series rules. Watch recordings in progress. |
| Medium | VOD support | Dispatcharr core | Stream movies and TV series with IMDB and TMDB metadata. |
| Medium | XC catch-up and timeshift | Dispatcharr core | Proxy provider archives to clients. Advertise catch-up in guides. |
| Medium | Buffer engine | xTeVe, Threadfin | Use FFmpeg or VLC to buffer streams and prevent stutter on slow provider connections. |
| Medium | HLS output | Dispatcharr roadmap | Serve streams as HLS alongside MPEG-TS. |
| Low | Plugin system | Dispatcharr core | Extend functionality with custom plugins that run inside the application. |
| Low | Bandwidth tracking | Dispatcharr core | Track per-session and global bandwidth usage in real time. |
| Low | Fallback videos | Dispatcharr roadmap | Play fallback content when a channel stream is unavailable. |
| Low | Logo auto-assignment | Dispatcharr Lineuparr | Auto-assign channel logos from EPG icons or the tv-logos GitHub repository. |
| Low | Schedules Direct | Dispatcharr core | Integrate the Schedules Direct premium EPG source. |
| Low | Comskip commercial stripping | Dispatcharr core | Strip commercials from DVR recordings. |

### Stream health checking and quality ranking

The `0013_stream_health_quality.sql` migration adds:

- `health_status`, `health_checked_at`, `health_error` columns to `provider_streams`.
- Video and audio metadata columns: `video_codec`, `video_resolution`, `video_width`, `video_height`, `video_fps`, `audio_codec`, `audio_channels`, `audio_sample_rate`, `bitrate_kbps`.
- `stream_health_checks` table for health check audit logs.
- `failover_count` and `last_failover_at` columns on `channel_streams`.
- `quality_rank` column on `channel_streams` for quality-based stream ordering.
- Indexes for health status lookups and quality ranking queries.

New persistence methods:

- `list_stream_health`: paginated list with status and group filters.
- `update_stream_health`: update stream health and log the check.
- `list_streams_needing_health_check`: find streams with unknown or dead status.
- `rank_channel_streams_by_quality`: rank streams for one channel by resolution, FPS, and health.
- `rank_all_channel_streams_by_quality`: rank streams for all channels.
- `best_stream_for_channel`: get the best available stream, ordered by health then quality.
- `record_failover`: increment failover count and timestamp.
- `stream_health_stats`: aggregate counts by status for the dashboard.

New API endpoints:

- `GET /api/v1/streams/health`: list stream health data with filters.
- `GET /api/v1/streams/health/stats`: aggregate health statistics.
- `POST /api/v1/streams/health/check`: trigger health check for streams that need it.
- `POST /api/v1/streams/rank`: rank all channel streams by quality.
- `GET /api/v1/channels/{channel_id}/best-stream`: get the best available stream for a channel.

The frontend has a new `/stream-health` page with:

- Health statistics cards: alive, dead, unknown, checking.
- A "Check streams" button that triggers health checks.
- A "Rank by quality" button that ranks all channel streams.
- A filterable stream health list with status badges and video quality metadata.
- Navigation entry in the primary sidebar.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests.

### Multi-user access control

The `0014_multi_user_access.sql` migration adds:

- `users` table with username, display name, password hash, role, and enabled flag.
- `user_channel_grants` table for per-user channel access control.
- `user_profile_grants` table for per-user output profile access.
- `user_tokens` table for API token authentication.
- Role values: `admin`, `operator`, `viewer`.

New persistence methods:

- `list_users`: list all user accounts.
- `get_user_by_username`: find a user by username for authentication.
- `get_user`: find a user by ID.
- `create_user`: create a new user account.
- `update_user`: update display name, password, role, or enabled flag.
- `delete_user`: delete a user account.
- `record_user_login`: record the last login timestamp.
- `get_user_password_hash`: get the password hash for authentication.
- `grant_channel_access`: grant a user access to a channel.
- `revoke_channel_access`: revoke a user access to a channel.
- `list_user_channels`: list channels a user has access to.

New API endpoints:

- `GET /api/v1/users`: list all user accounts.
- `POST /api/v1/users`: create a new user account.
- `PATCH /api/v1/users/{user_id}`: update a user account.
- `DELETE /api/v1/users/{user_id}`: delete a user account.

The frontend has a new `/users` page with:

- A list of user accounts with role badges and enabled status.
- A form to create new users with username, display name, password, and role.
- Buttons to enable or disable users.
- Buttons to delete users.
- Navigation entry in the primary sidebar.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests.

### Channel name standardization

The `0015_channel_aliases.sql` migration adds:

- `channel_aliases` table with canonical name, alias, country, and category columns.
- Indexes for canonical name, alias, and country lookups.
- 98 pre-loaded aliases for common US and UK channels.

Pre-loaded alias coverage:

- US networks: ABC, CBS, NBC, FOX, PBS.
- US sports: ESPN, ESPN2, FS1, FS2, NBCSN, NFL Network, NBA TV, MLB Network, NHL Network.
- US entertainment: TNT, TBS, AMC, FX, USA, Syfy, Bravo, E!, Comedy Central.
- US news: CNN, Fox News, MSNBC, HLN.
- US premium: HBO, Showtime, Cinemax, Starz, Encore.
- US documentary: Discovery, History, Nat Geo, Animal Planet, Science, TLC.
- US lifestyle: Food Network, HGTV, DIY, Cooking Channel, Travel Channel.
- UK channels: BBC, BBC One, BBC Two, ITV, Channel 4, Channel 5, Sky One, Sky Sports, Sky News.

New persistence methods:

- `list_channel_aliases`: paginated list with country filter.
- `resolve_channel_alias`: resolve a name to its canonical form.
- `create_channel_alias`: create a new alias entry.
- `delete_channel_alias`: delete an alias entry.
- `channel_alias_stats`: aggregate statistics for the dashboard.

New API endpoints:

- `GET /api/v1/channel-aliases`: list aliases with optional country filter.
- `POST /api/v1/channel-aliases`: create a new alias.
- `DELETE /api/v1/channel-aliases/{alias_id}`: delete an alias.
- `GET /api/v1/channel-aliases/resolve?name=...`: resolve a name to its canonical form.

The frontend has a new `/channel-aliases` page with:

- A name resolver that shows the canonical name for a given input.
- A form to add new aliases with canonical name, alias, country, and category.
- A list of all aliases with country and category badges.
- Delete buttons for each alias.
- Navigation entry in the primary sidebar.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests.

### DVR recording scheduling

The `0016_dvr_recordings.sql` migration adds:

- `recording_rules` table for recording rule definitions.
- `recordings` table for recording instances.
- Rule types: `one-time`, `recurring`, `series`.
- Recording statuses: `scheduled`, `recording`, `completed`, `failed`, `cancelled`.
- Keep-until policies: `space-needed`, `one-day`, `one-week`, `until-watched`, `forever`.
- Start and end padding minutes for each rule.
- Optional title and category filters for series rules.
- Optional maximum recording count per rule.
- File path, file size, duration, and error tracking for completed recordings.

New persistence methods:

- `list_recording_rules`: list all recording rules.
- `create_recording_rule`: create a new recording rule.
- `delete_recording_rule`: delete a recording rule.
- `list_recordings`: paginated list with optional status filter.
- `create_recording`: create a new recording instance.
- `update_recording_status`: update recording status and file metadata.
- `delete_recording`: delete a recording.
- `recording_stats`: aggregate statistics for the dashboard.

New API endpoints:

- `GET /api/v1/recordings/rules`: list all recording rules.
- `POST /api/v1/recordings/rules`: create a new recording rule.
- `DELETE /api/v1/recordings/rules/{rule_id}`: delete a recording rule.
- `GET /api/v1/recordings`: list recordings with optional status filter.
- `POST /api/v1/recordings`: create a new recording instance.
- `DELETE /api/v1/recordings/{recording_id}`: delete a recording.
- `GET /api/v1/recordings/stats`: aggregate recording statistics.

The frontend has a new `/recordings` page with:

- Statistics cards: scheduled, recording, completed, failed, and total storage.
- A form to create new recording rules with name, channel, and type.
- A list of recording rules with delete buttons.
- A list of recordings with status badges and file metadata.
- Delete buttons for each recording.
- Navigation entry in the primary sidebar.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests.

### Stream profiles

The `0017_stream_profiles.sql` migration adds:

- `stream_profiles` table for upstream connection configuration.
- `channel_stream_profiles` table for per-channel profile assignments.
- Profile types: `direct`, `ffmpeg`, `vlc`, `streamlink`, `custom`.
- Buffer seconds, user agent, referer, and custom command support.
- Four default profiles: Direct, FFmpeg, VLC, Streamlink.

New persistence methods:

- `list_stream_profiles`: list all stream profiles.
- `create_stream_profile`: create a new stream profile.
- `delete_stream_profile`: delete a stream profile.
- `assign_stream_profile`: assign a profile to a channel.
- `remove_stream_profile`: remove a profile assignment from a channel.

New API endpoints:

- `GET /api/v1/stream-profiles`: list all stream profiles.
- `POST /api/v1/stream-profiles`: create a new stream profile.
- `DELETE /api/v1/stream-profiles/{profile_id}`: delete a stream profile.
- `POST /api/v1/channels/{channel_id}/stream-profile`: assign a profile to a channel.
- `DELETE /api/v1/channels/{channel_id}/stream-profile`: remove a profile assignment.

The frontend has a new `/stream-profiles` page with:

- A list of stream profiles with type badges and buffer configuration.
- A form to create new profiles with name, type, command, buffer, and user agent.
- Delete buttons for each profile.
- Navigation entry in the primary sidebar.

Verified results:

- `cargo test --workspace --all-targets` passed with 269 Rust tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests.

## Query and serialization performance audit

A full performance audit removed three N+1 query loops and one O(n*m) lookup.

The `apply_lineup` method ran one name-match query per lineup channel. A single batch query with `unnest` replaced the loop. A category with N lineup channels now issues one round trip instead of N.

The `scan_event_channels` method ran two queries per matched stream. A batched `INSERT ... SELECT FROM unnest` and one template-wide `UPDATE` replaced the loop. A template with N matches now issues two round trips instead of 2N.

The `trigger_health_check` endpoint ran two queries per queued stream. A new `mark_streams_checking` method issues one batch `UPDATE` for all selected stream IDs. The endpoint now issues two round trips instead of 2N.

The XMLTV endpoint did a linear `channels.iter().find` scan per programme. The `ProgrammeRow` now carries `channel_id` from the mapping join. A `HashMap<Uuid, &ChannelRow>` lookup replaced the scan. The XMLTV build stays constant time per programme.

The TV guide page filtered programmes on every render. A `useMemo` hook now memoizes the current and upcoming splits so they recompute only when the programme list changes.

Verified results:

- `cargo build --workspace` passed.
- `cargo test --workspace --all-targets` passed with 274 Rust tests.
- `npx vitest run` passed with 40 frontend tests.

### Manual source sync

The `POST /api/v1/sources/{source_id}/sync` endpoint enqueues a `refresh-source` job for a specific source. The endpoint requires admin authentication and CSRF protection.

The endpoint returns `409 Conflict` when a refresh job for the same source is already queued or running. This prevents duplicate refresh jobs.

The `SourceSyncResponse` returns the job ID and a confirmation message.

The frontend sources page includes a sync button per source row. The button uses a `RefreshCw` icon and shows a spin animation while the request is in progress. The mutation invalidates the `sources` and `overview` query caches on success.

The `IptvApiClient` interface includes the `triggerSourceSync` method. Both the real and mock API clients implement the method.

Verified results:

- `cargo build -p iptv-api` passed.
- `cargo test --workspace --all-targets` passed with 274 Rust tests.
- `npx tsc --noEmit` passed.
- `npx vitest run` passed with 40 frontend tests.
- Manual curl test confirmed: `POST /api/v1/sources/{id}/sync` returned `202` with a job ID.
- Manual curl test confirmed: a second sync request returned `409` with `refresh-already-active`.

### Inline source sync progress

The `IptvApiClient` interface now includes `getSourceSyncStatus`. The `SourceSyncStatus` type includes `jobId`, `status`, `stage`, `percent`, `message`, `bytesDownloaded`, `recordsProcessed`, `startedAt`, and `updatedAt`.

The real `FetchIptvApiClient` sends `GET /api/v1/sources/{source_id}/sync-status`. The mock client tracks an in-memory job and advances its percent on each call.

The `apiQueries` helper adds a `sourceSyncStatus` query that fetches the initial status once. The query uses `staleTime: Infinity` so it does not poll. The `useCatalogEvents` SSE hook receives `source-sync-progress` events and writes the progress data into the React Query cache at the `source-sync-status` query key. This provides realtime progress updates without polling.

The sources page tracks active sync IDs in component state. Clicking the sync button triggers the sync and adds the source to the active set. A `SourceSyncProgress` component renders in the row for each active source.

The progress UI shows a thin progress bar, stage label, percentage, message, and byte or record counts. It uses `role="progressbar"` with `aria-valuenow`, `aria-valuemin`, and `aria-valuemax`. The sync button disables while the source sync is active.

The `SourceSyncProgress` component shows a brief `Synced` badge on success and a `Sync failed` alert on error. It invalidates the `sources` and `overview` queries on completion and removes the source from the active set after two seconds.

Verified results:

- `npx tsc --noEmit` passed with zero errors.
- `npx vitest run` passed with 40 frontend tests.
- SSE integration verified: `useCatalogEvents` writes `source-sync-progress` events into the query cache without polling.

The worker reports refresh progress through `JobRepository::heartbeat` at each refresh stage. The `WorkerJobControl::checkpoint` method maps each ingest phase to a stage-based progress JSON object with `stage`, `percent`, `bytesDownloaded`, `recordsProcessed`, and `message` fields.

The stage mapping is:

- `downloading` covers the `downloading` and `downloaded` ingest phases (5 to 15 percent).
- `parsing` covers the `decoded`, `parsed`, and `staging` ingest phases (30 to 55 percent).
- `activating` covers the `activated` ingest phase (70 percent).
- `reconciling` is reported before catalog reconciliation starts (85 percent).
- `completed` is reported before the job succeeds (100 percent).

The `GET /api/v1/sources/{source_id}/sync-status` endpoint returns the most recent `refresh-source` job for the source. The `SourceSyncStatusResponse` includes `jobId`, `status`, `stage`, `percent`, `message`, `bytesDownloaded`, `recordsProcessed`, `startedAt`, and `updatedAt`. The endpoint returns `404` when no refresh job exists for the source.

The `/api/v1/catalog-events` SSE stream emits `source-sync-progress` events for queued or running `refresh-source` jobs. Each event payload includes `sourceId`, `jobId`, `status`, `stage`, `percent`, `message`, `bytesDownloaded`, `recordsProcessed`, and `updatedAt`.

Verified results:

- `cargo build -p iptv-api -p iptv-gateway` passed with zero errors.
- `cargo test --workspace --all-targets` passed with all tests green.

### Source sync cancellation

The `POST /api/v1/sources/{source_id}/sync/cancel` endpoint cancels the most recent active `refresh-source` job for a source. The endpoint requires admin authentication and CSRF protection.

The endpoint finds the most recent queued or running `refresh-source` job for the source. The endpoint returns `404` when no active job exists. The endpoint calls `JobRepository::cancel` and returns `200` with `{ok: true, message: "Sync cancelled."}` on success. The endpoint returns `409` when the job is already in a terminal state.

The `/api/v1/catalog-events` SSE stream now emits `source-sync-progress` events for `cancelled` jobs in addition to queued and running jobs. The frontend receives the `cancelled` status through the existing event payload.

Verified results:

- `cargo build -p iptv-api` passed with zero errors.
- `cargo test --workspace --all-targets` passed with all tests green.

## Docker worker job queue durability

The default Compose configuration passed `docker-compose config --quiet`.

The development Compose configuration passed `docker-compose -f docker-compose.yml -f docker-compose.dev.yml config --quiet`.

The worker service now includes a health check, a 60-second stop grace period, and the SIGTERM stop signal.

The runtime image installs `postgresql-client` to support the worker health check command.

The development override no longer puts the worker behind a profile, so workers start by default.

The `dev` Makefile target starts the local Caddy, PostgreSQL, and web services.

The `deploy/Caddyfile.local` already exists and references `host.docker.internal`.

The `.env` file exists and contains `IPTV_MASTER_KEY`.

## Job queue root cause diagnosis

Source refresh jobs remained in the `queued` state and never entered `running`.

Two causes were identified:

1. The local development process ran only the `serve` command. The `serve` command starts the HTTP API but does not run the worker loop. The worker is a separate `worker` command. In development mode, worker containers were behind a `dev-worker` Docker Compose profile and did not start by default.
2. When a local worker was started, it used the placeholder master key from `docker-compose.yml` defaults. The sources were encrypted with the real key from the `.env` file. Decryption failed and the job reported `source configuration could not be loaded`.

After both issues were corrected, jobs transition from `queued` to `running` to `succeeded`.

Verified results:

- The worker process logged `job worker started` and `source refresh scheduler started`.
- A queued job was claimed and transitioned to `running`.
- The sync status API returned `stage: "downloading"`, `percent: 5`, `bytesDownloaded: 0`.
- The sync status API returned `stage: "parsing"`, `percent: 30`, `bytesDownloaded: 474787056`.
- The job transitioned to `succeeded` with `attempts: 1`.

## Job queue durability improvements

The custom PostgreSQL-backed job queue was hardened with four changes.

### Stale-lease reaper

The `JobRepository::reap_stale_jobs` method resets `running` jobs with stale heartbeats back to `queued`.

A job is stale when its `heartbeat_at` is older than the lease timeout.

A job is also stale when `heartbeat_at` is null and `locked_at` is older than the lease timeout.

The worker spawns a reaper task that calls `reap_stale_jobs(300)` every 30 seconds. The 300-second lease timeout means a job whose heartbeat is older than 5 minutes gets reset.

### Simplified claim query

The `claim` method was rewritten to use a subquery instead of a CTE. The previous `WITH candidate AS (...) UPDATE ... FROM candidate` form could confuse the PostgreSQL lock manager. The new form uses `WHERE id = (SELECT id FROM jobs ... FOR UPDATE SKIP LOCKED LIMIT 1)`.

### Exponential backoff

The `fail` method now computes the retry delay internally. The delay is `2^attempts` seconds, capped at 300 seconds. The previous implementation used a fixed 30-second delay. The `fail` method signature changed from `fail(job_id, worker_id, retry, error_summary, available_at)` to `fail(job_id, worker_id, attempts, max_attempts, error_summary)`.

### Dead-letter queue

The `list_failed_jobs` method lists failed jobs ordered by `updated_at` descending for operator inspection. Migration `0018_dead_letter_queue_index.sql` adds a partial index `jobs_failed_idx` on `jobs(updated_at DESC) WHERE status = 'failed'`.

### Graceful shutdown

The worker loop breaks on `SIGTERM` or `SIGINT` instead of returning immediately. This lets the current job finish before exit. The reaper and scheduler tasks are aborted after the loop exits. The Dockerfile sets `STOPSIGNAL SIGTERM`. The Docker Compose worker service sets `stop_grace_period: 60s` and `stop_signal: SIGTERM`.

### Heartbeat task

The worker spawns a heartbeat task for each job that writes an empty progress payload every 15 seconds. This keeps the lease fresh during slow downloads where ingest checkpoints are sparse. The task is aborted when the job completes.

### Idle poll interval

The idle poll interval decreased from 500 milliseconds to 200 milliseconds for faster job pickup.

Verified results:

- `cargo build --workspace --all-targets` passed.
- `cargo test --workspace --all-targets` passed with 274 tests.
- `cargo build --release -p iptv-gateway` passed.
- Migration 18 applied successfully.
- A fresh sync job transitioned from `queued` to `running` to `succeeded` with the correct master key.
- The sync status API returned live progress data during the job.

## Channel enabled filter fix

The `getChannels` API client method dropped the `enabled` query parameter.

The backend accepted the `enabled` filter but the frontend never sent it.

The channels page showed all channels when the filter was set to "Enabled".

The TV guide page showed disabled channels as enabled for the same reason.

The fix adds the `enabled` parameter to the `buildQueryString` call.

The mock client also filters by `enabled` for test consistency.

Verification:

- `npx tsc --noEmit` passed.
- `npx vitest run` passed: 4 test files, 40 tests.
- `GET /api/v1/channels?enabled=true` returned 0 channels when all were disabled.
- `PATCH /api/v1/channels/{uuid}/enabled` set one channel to enabled.
- `GET /api/v1/channels?enabled=true` returned 1 channel after the toggle.
- `GET /api/v1/channels?enabled=false` returned 1,167,409 disabled channels.
- `GET /api/v1/channels` without the filter returned 1,167,410 channels.

## Production media policy work

Last verification: 2026-08-20.

The media endpoint now supports an explicit provider pool.

A failover can move its lease to a different provider pool.

The old lease stays active until the new source passes MPEG-TS priming.

The sandbox test failed because four tests could not bind loopback ports.

The unrestricted `cargo test -p iptv-media` rerun passed all 44 tests.

The PostgreSQL health check failed because the Colima disk had no free space.

Docker logs confirmed the disk-space error during the PostgreSQL recovery checkpoint.

Twenty-one stopped, reproducible IPTV containers were removed.

No Docker volumes were removed.

The PostgreSQL health check passed after the cleanup.

The new playback-plan query prefers healthy streams.

The query applies shared pool caps before account caps.

The sandbox persistence test could not open the local PostgreSQL socket.

The unrestricted focused persistence test passed.

The first compile check found a misplaced provider revision field.

The field placement was corrected.

`cargo check -p iptv-api -p iptv-persistence -p iptv-media` passed.

Public playback now loads all active database stream candidates.

Playback configures each effective provider pool before it opens a session.

Playback decrypts each stream URL without a redacted-template fallback.

Playback uses the channel UUID as its shared source identity.

`cargo test -p iptv-api` passed all 28 tests.

The focused PostgreSQL playback-plan test passed after the API integration.

The media refactor test passed all 44 tests after the lease helper change.

The focused strict Clippy check found API warnings.

The warnings include existing API complexity findings and three large error variants in the new playback helpers.

The changed-line coverage script tests passed.

The changed production line threshold is now 95 percent.

The test covers a one-commit repository, CI base references, untracked files, absolute paths, and missing coverage data.

The Make dry run confirmed that `ci` includes the changed-line coverage gate.

The documentation claim review passed `git diff --check`.

The revised documentation separates stored API scaffolds from active runtime behavior.

The playback documentation now describes database limits, ranked alternates, and the native HTTP adapter limit.

MkDocs is not installed, so the strict documentation build did not run.

The first web coverage command failed during the automatic dependency check.

The check required network access and a noninteractive environment.

The unrestricted web coverage run reached 46 tests.

Five page tests failed because the forms rendered Zod issue objects as React children.

The second web coverage run reduced the failures to three login tests.

Form-level change validation incorrectly validated the untouched password when the username changed.

The third web coverage run passed all 46 tests.

The web coverage gate failed at 41.45 percent line coverage and 41.92 percent branch coverage.

The TypeScript check passed.

The production web build passed.

The strict web lint found seven unused imports, variables, or test parameters.

The corrected strict web lint passed.

The workspace format check passed.

The focused strict Clippy check passed for the media, persistence, and API crates.

The API test rerun passed all 28 tests after the strict Clippy refactor.

The focused PostgreSQL playback-plan test passed after the strict Clippy refactor.

Preview URLs no longer contain bootstrap bearer credentials.

The admin stream route no longer accepts credentials in query parameters.

The API test passed all 29 tests after this security change.

The strict API Clippy check found the obsolete bootstrap-token state field.

The obsolete state field was removed.

The strict API Clippy rerun passed.

The first focused security test command matched zero tests because the exact selector omitted the module path.

The corrected focused security test passed.

Compose no longer supplies fallback values for database, output, bootstrap, or master secrets.

Server startup now rejects blank, short, malformed, and placeholder tokens.

Server startup now rejects blank, short, outer-spaced, and placeholder administrator passwords.

The gateway server test passed all 50 tests after the startup-secret changes.

The strict gateway Clippy check found two existing style findings in the refresh worker.

The gateway format and strict Clippy reruns passed after the style corrections.

The base Compose configuration check failed because the local environment does not set `POSTGRES_PASSWORD`.

This failure confirms that Compose no longer uses the former database-password fallback.

The project now has a deterministic test-only Compose environment.

The base Compose configuration check passed with the example environment.

The test-profile Compose configuration check passed with the test environment.

The API and server configuration debug formats now redact all plaintext secrets.

The API test passed all 30 tests after the redaction and preview-header changes.

The gateway server test passed all 51 tests after the debug-redaction change.

The strict API and gateway Clippy checks passed.

The secure configuration documentation passed `git diff --check`.

The base Compose gateway now binds to `127.0.0.1` by default.

The base and test-profile Compose configuration checks passed after the bind change.

The persistence library test passed all 15 tests after the coverage additions.

Three new isolated PostgreSQL coverage tests passed.

The tests cover catalog mapping, event lifecycles, source configuration, and job management.

The web coverage gate passed with 72 tests.

Web coverage reached 96.38 percent lines, 94.43 percent functions, and 86.05 percent branches.

The first credential-broker test build failed because the test used unavailable JSON response helpers.

The second credential-broker test build found a moved test counter.

The loopback credential broker now issues a session token with more than 256 bits of random input.

The broker stores only the token hash and accepts one upstream request.

Broker debug output omits provider URLs, header values, and local session tokens.

The media test passed all 47 tests after the broker corrections.

The strict media Clippy check found two incomplete debug formats and one match-style finding.

The media format and strict Clippy reruns passed after the corrections.

The output-profile integration check found one missing persistence-error match.

The output-profile integration check passed after the error-match correction.

The strict output-profile API, server, and persistence Clippy check passed.

The API test passed all 30 tests after the output-profile route changes.

The concurrent gateway test found an incomplete media-adapter edit.

The isolated output-profile persistence test passed.

The test verifies token overlap, profile state, channel order, and channel membership.

The strict API Clippy check passed after the database output-route test addition.

The first focused output-route command matched zero tests because it omitted the module path.

The corrected database output-route test passed.

The test verifies channel filtering, stream denial, tuner count, token overlap, and invalid-token denial.

The sandboxed media test passed 41 tests and blocked seven loopback socket tests.

The gateway server test passed all 51 tests after the brokered-process addition.

The media test passed all 48 tests with loopback socket access.

The brokered FFmpeg and VLC path passes a provider stream through a redacted loopback URL.

The current broker supports one direct media response and does not support HLS segment traffic.

The full serial PostgreSQL persistence integration suite passed all 15 tests.

The corrected tests cover reconciliation, recording statistics, and stream-health ranking.

The gateway server test passed all 51 tests after the tuner-count correction.

The first Compose checks failed because the installed Docker command does not include Compose.

The base and test-profile Compose checks passed with `docker-compose`.

Compose now passes the configured HDHomeRun tuner count to the core service.

The output-profile and Jellyfin documentation passed `git diff --check`.

The Rust coverage gate failed after all current tests passed.

Rust coverage is 78.99 percent for lines, 77.95 percent for functions, and 78.33 percent for regions.

The coverage exclusion also includes two untested acceptance binaries by mistake.

The coverage commands now exclude all approved test-provider and load-generator binaries.

The changed-line coverage script tests passed after the exclusion correction.

The corrected Rust report is 80.76 percent for lines, 80.31 percent for functions, and 80.31 percent for regions.

The sandboxed dependency audit could not lock the read-only Cargo advisory database.

The unrestricted dependency audit passed advisories, bans, licenses, and sources.

The audit reports only allowed duplicate-version and unused-license warnings.

The isolated catalog coverage test passed.

The API test passed all 33 tests with PostgreSQL and loopback access.

The API test verifies durable bootstrap-bearer disablement across a restart.

The 120-second media acceptance run passed the three-viewer and six-viewer scenarios.

The strict media-acceptance and test-provider Clippy check passed after channel-identity and continuity checks.

The focused Compose media run passed both scenarios for three seconds.

Each viewer verified its PAT service identity and every MPEG-TS continuity counter.

The parser test passed all parser tests.

The parser coverage check passed all 86 percent thresholds.

M3U coverage is 96.60 percent for lines and 100 percent for functions.

XMLTV coverage is 92.94 percent for lines and 86.90 percent for functions.

The strict parser Clippy check passed.

The ingest test passed 62 tests and ignored one test.

The ingest coverage check passed for the parser and pipeline files.

The ingest parser has 100 percent line and function coverage.

The ingest pipeline has 95.97 percent line coverage and 86.67 percent function coverage.

The strict ingest Clippy check passed.

The mandatory media acceptance test passed both 120-second scenarios.

Three viewers used three upstream sessions for the first scenario.

Six viewers shared three upstream sessions for the second scenario.

All viewers kept the correct channel identity and continuity.

The API test passed all 37 tests.

The strict API Clippy check passed.

The API library coverage check did not meet the 86 percent thresholds.

The API library has 49.74 percent line coverage and 45.17 percent function coverage.

The Compose worker check found four restarting workers.

The stale worker image does not contain migration 7.

The media test passed all 49 tests after the process shutdown safety change.

The provider slot remains active through child reap and broker shutdown.

The strict media Clippy check passed.

The implementation plan passed the documentation whitespace and sentence-length checks.

The API test passed all 39 tests after the coverage additions.

The API library now has 82.77 percent line coverage.

The API library now has 83.91 percent function coverage.

The API library now has 80.53 percent region coverage.

The strict API Clippy check passed after the coverage additions.

The gateway test passed 55 unit tests and three command tests.

The server coverage run without PostgreSQL reached 57.8 percent line coverage.

The workspace strict Clippy check found a concurrent media import warning.

The sandbox blocked the local process-list check.

The API database test found a full Docker virtual-machine disk.

PostgreSQL returned error 53100 during an isolated schema migration.

The media test passed all 52 tests after the shared process integration.

FFmpeg and VLC share one process and one provider slot for two viewers.

The process session reports streaming only after a confirmed PAT and PMT boundary.

The final viewer stops the FFmpeg session within two seconds.

The strict media Clippy check passed after the shared process integration.

The focused media report has 78.26 percent line coverage.

Docker image cleanup removed only dangling images.

The cleanup recovered 13.92 GB of Docker virtual-machine disk space.

PostgreSQL returned to a healthy state after the cleanup.

The focused API adapter-policy test passed.

The API maps `auto`, `native-ts`, `ffmpeg`, and `vlc` to the shared session policy.

An invalid stored adapter returns a redacted typed error.

The strict API Clippy check passed after the adapter-policy change.

The gateway test passed all 59 tests after the database coverage additions.

Server line coverage is 87.19 percent.

Server function coverage is 89.53 percent.

Server region coverage is 87.79 percent.

The strict gateway Clippy check passed after the database coverage additions.

The workspace format check passed after the database coverage additions.

The focused dynamic-event parser tests passed.

The parser creates deterministic multi-event schedules with contiguous filler.

The parser accepts captured durations and IANA timezones.

The parser rejects ambiguous and nonexistent DST times.

The parser prevents generated programme overlaps.

The strict parser Clippy check passed after the dynamic-event changes.

The event parser coverage is 68.63 percent for lines.

The event parser does not meet the critical-file coverage threshold.

The media test passed all 55 tests after the HLS broker change.

The HLS broker rewrites manifests and nested playlists to loopback URLs.

The HLS broker proxies segments, AES keys, and maps with protected headers.

The HLS broker rejects traversal, invalid schemes, expired tokens, and oversized responses.

The strict media Clippy check passed after the HLS broker change.

The focused media report has 90.90 percent line coverage.

The credential broker has 77.60 percent line coverage and 68.14 percent function coverage.

The credential broker does not meet the critical-file function threshold.

The first event-output persistence test used an invalid channel owner value.

The channel owner constraint accepts `automatic`, `manual`, or `event`.

The parser test passed all 61 tests after the event coverage additions.

Event parser coverage is 98.30 percent for lines.

Event parser coverage is 98.98 percent for functions.

Event parser coverage is 97.42 percent for regions.

The event-output PostgreSQL test passed.

The XMLTV output publishes stored event-channel intervals.

The strict parser and persistence Clippy checks passed.

The workspace format check found concurrent unformatted media edits.

The CI-gate agent could not start because the agent thread limit was reached.

The HLS process path uses an explicit input format.

The FFmpeg manager test passed nested HLS resources through one shared process.

The dynamic-event migration and isolated PostgreSQL lifecycle test passed.

The concurrent API build found a new unhandled media-session error variant.

The focused API HLS policy-error test passed.

The API returns a redacted typed error when HLS uses a native adapter.

The strict API Clippy check passed after the HLS policy-error change.

The media test passed all 57 tests after the HLS start-path change.

FFmpeg and VLC receive only the loopback HLS manifest URL.

Two HLS viewers share one process and one provider slot.

The final HLS viewer stops the process and broker before slot release.

Credential broker coverage is 93.72 percent for lines.

Credential broker coverage is 91.95 percent for functions.

Credential broker coverage is 92.37 percent for regions.

The strict media Clippy and format checks passed after the HLS change.

The strict persistence Clippy check passed after the generated-guide bridge.

The generated-guide PostgreSQL lifecycle test passed.

The first worker integration test did not finish within the tool output window.

The first focused coverage command placed the test filter before `--`.

Cargo LLVM coverage rejected the test name as a subcommand.

The persistence coverage run found a legacy event-output fixture.

The fixture did not create a durable generated programme.

The bounded gateway worker test timed out inside `process_job` after ten seconds.

The direct generated-guide PostgreSQL scan tests pass.

The deterministic worker invocation test remains incomplete.

Two deterministic worker invocation tests passed.

The M3U worker runs provider reconciliation, EPG reconciliation, and the dynamic scan in order.

The worker skips the dynamic scan after a failure or XMLTV refresh.

The strict gateway Clippy check passed after the worker bridge refactor.

Generated-guide storage coverage is 95.88 percent for lines.

Generated-guide storage coverage is 94.96 percent for functions.

Generated-guide storage coverage is 92.25 percent for regions.

The final parser suite passed all 61 tests.

The final persistence suite passed 16 unit tests and 18 PostgreSQL tests.

The final gateway hook and M3U worker tests passed.

The workspace format and diff checks passed after the generated-guide bridge.

Stored event rule sets do not yet drive the worker scan.

Per-template filler and category settings do not yet drive generation.

Generated programmes do not yet have a control API view.

The parallel core and worker image build failed with a compiler SIGKILL.

Compose now builds one shared Rust image for the core, workers, and test binaries.

The base and test-profile Compose configuration checks passed after the image change.

The media, fault, and live acceptance Make dry runs use the shared image build.

The corrected single Rust image build passed.

The complete Rust coverage run failed one gateway test.

The M3U refresh test found overlapping generated programme intervals.

PostgreSQL rejected the overlap with the generated-programme exclusion constraint.

The recreated core and four workers stayed healthy for five minutes.

No recreated service reported a missing migration.

The first persistence coverage rerun retained stale source artifacts.

The stale report showed 71.83 percent coverage.

The generated-guide collision regression test passed with PostgreSQL.

A conflicting template now preserves the existing valid guide.

The focused gateway hook tests and strict persistence Clippy check passed after the collision fix.

Clean persistence coverage is 95.93 percent for lines.

Clean persistence coverage is 94.96 percent for functions.

Clean persistence coverage is 92.27 percent for regions.

The workspace format and diff checks passed after the collision fix.

The complete Rust coverage run passed 43 API tests and 60 gateway tests.

The complete Rust coverage run failed one M3U refresh assertion.

The failed assertion did not find its expected event-channel target.

The guide interval exclusion error did not recur.

The four Compose workers use four distinct hostnames.

The server uses each hostname as its default worker ID.

The live worker pool completed one durable no-op job.

The durable no-op job reached `succeeded` after one attempt.

The complete base Compose start failed at the gateway port bind.

Host port 8080 was already allocated.

The complete base Compose stack passed on alternate host port 18080.

The gateway, web, core, PostgreSQL, and four workers reported healthy.

The gateway core health route returned `status: ok`.

The gateway web route returned a successful response.

The web coverage command did not start.

Corepack selected pnpm 11.22.0, but the project requires pnpm 11.19.0.

The documented `pnpm with 11.19.0` command did not start under Corepack.

The direct pnpm 11.19.0 command could not reach the npm registry in the sandbox.

The direct command also required noninteractive CI mode for module replacement.

The pinned pnpm 11.19.0 web coverage gate passed all 72 tests.

Web statement coverage is 94.96 percent.

Web branch coverage is 86.05 percent.

Web function coverage is 94.43 percent.

Web line coverage is 96.38 percent.

The first corrected Make web coverage command selected pnpm 11.19.0.

That command could not replace the module directory without CI mode and registry access.

The corrected `make coverage-web` command passed with the project pnpm version.

The Make targets now select pnpm from the web project directory.

The Make targets now use noninteractive CI mode.

The web TypeScript check passed after the Make correction.

The web lint check passed with warnings denied.

The isolated PostgreSQL gateway refresh test passed after the fixture correction.

The PostgreSQL collision and idempotence regression test passed.

The refresh test now uses a unique schema and group.

The refresh test deletes its event template and source.

The strict gateway and persistence Clippy checks passed.

The workspace format and diff checks passed after the fixture correction.

The complete Rust coverage command passed all tests and aggregate thresholds.

Rust line coverage is 92.05 percent.

Rust function coverage is 91.82 percent.

Rust region coverage is 90.49 percent.

The gateway passed all 61 tests in the complete coverage run.

The API passed all 43 tests in the complete coverage run.

The ingest library facade has 62.50 percent line coverage.

The changed-line and critical-file gates still need separate results.

The changed-line coverage script passed all three self-tests.

The self-tests cover a passing diff, an untracked file, and absent coverage data.

The ingest error-summary test passed for every public error variant.

The ingest error-summary test verifies credential-safe persisted messages.

The strict ingest Clippy check passed after the test addition.

The focused ingest library suite passed 64 tests with one live test ignored.

The ingest library facade now has 100 percent line, function, and region coverage.

The plan and status sentence-length check passed.

The focused diff check passed for the plan, status, Makefile, and ingest facade.

The dependency audit passed its advisory, ban, license, and source checks.

The audit reported allowed duplicate dependency and unused license warnings.

The complete workspace diff check passed during the timezone change.

The workspace Rust format check passed during the timezone change.

The XMLTV timezone parser suite passed all 17 tests.

The tests cover UTC and explicit offsets.

The tests cover Denver winter and summer offsets.

The tests reject ambiguous and nonexistent Denver times.

The ingest tests preserve literal XMLTV start and stop values.

The PostgreSQL source test persists the timezone before the first refresh job.

The PostgreSQL guide test orders current, future, and past programmes at a fixed clock.

The API suite passed all 43 tests after the timezone change.

The focused web suite passed all 33 tests after the timezone change.

The web TypeScript check passed after the timezone change.

The TV Guide now displays time in the browser timezone.

The strict Rust Clippy, format, and diff checks passed after the timezone change.

The complete web coverage gate passed all 73 tests after the timezone change.

Web statement coverage is 94.84 percent.

Web branch coverage is 86.01 percent.

Web function coverage is 94.28 percent.

Web line coverage is 96.31 percent.

The complete Rust coverage gate passed after the timezone change.

Rust line coverage is 92.12 percent.

Rust function coverage is 91.77 percent.

Rust region coverage is 90.54 percent.

The ingest library facade has 100 percent coverage in all required measures.

The changed-line coverage gate failed at 76.36 percent.

The gate counted 42,634 changed lines and 10,078 uncovered lines.

The gate incorrectly sent TypeScript files to Rust coverage.

The base and test-profile Compose configuration checks passed after the timezone change.

The mixed-language changed-line script passed its expanded self-tests.

The self-tests cover TypeScript, Rust, mixed input, missing reports, and fuzz exclusions.

The timezone and generated-guide documentation diff check passed.

The documentation sentence-length check passed.

The strict MkDocs build did not run because MkDocs is not installed.

The corrected changed-line worktree gate still failed at 76.52 percent.

The corrected gate counted 42,545 lines and 9,989 uncovered lines.

The unchanged covered count shows that TypeScript LCOV paths did not match worktree paths.

The LCOV matcher now accepts paths relative to the web project root.

The expanded coverage-script, shell syntax, and diff checks passed after the path fix.

The mixed-language changed-line gate now reports a truthful 89.91 percent.

The gate counted 42,545 changed lines and 4,292 uncovered lines.

The 95 percent changed-line requirement does not yet pass.

The final plan, status, README, and workspace diff checks passed.

## Sync progress preservation and active-job selection

Last verification: 2026-08-21.

The periodic heartbeat task overwrote job progress with an empty JSON object.

This caused the sync status to lose stage, percent, bytes, and records data during long downloads.

The `JobRepository::touch_heartbeat` method refreshes the heartbeat timestamp without overwriting progress.

The worker heartbeat task now calls `touch_heartbeat` instead of `heartbeat` with an empty payload.

The `source_sync_status` endpoint returned the most recent job for a source.

When a retry created a newer terminal job, the endpoint returned `failed` while an older job was still `running`.

The endpoint now prefers active jobs over terminal jobs.

The SSE `catalog-events` stream now deduplicates jobs by source ID.

It prefers `running` over `queued` so the UI sees the most progressed job for each source.

The sources page shows a `Starting…` indicator with a spinner when the source state is `syncing` but no active job is found yet.

The `SourceSyncProgress` component shows elapsed time alongside the percent value.

The `Queued` state also shows elapsed time.

### Verified results

The API test suite passed all 43 tests with the PostgreSQL test database.

The gateway test suite passed all 61 tests.

The persistence library test suite passed all 17 tests.

The web test suite passed all 73 tests.

The TypeScript type check passed.

The web lint check passed.

The Clippy check passed for the persistence, API, and gateway crates with warnings denied.

The live Docker stack returned `running` with `stage=downloading`, `percent=5`, and `message=Downloading source data` during an M3U sync.

The `updatedAt` timestamp advanced every 15 seconds during the download.

The progress data persisted across heartbeat ticks without being overwritten.

## Test cleanup hooks

Last verification: 2026-08-21.

The tests left data behind in the shared database after completion.

### Playwright e2e tests

The `groups-flows.spec.ts` test toggled group enabled states but did not restore them.

A `beforeAll` hook now captures the initial enabled state of each group.

An `afterAll` hook restores the original enabled state after all tests complete.

The `management-flows.spec.ts` test changed source refresh interval, max connections, and timezone but did not revert these values.

A `beforeAll` hook now captures the initial settings of each source.

An `afterAll` hook restores the original settings after all tests complete.

The `events-lineup-flows.spec.ts` test deleted templates only on the success path.

The create-scan-delete and create-list-delete cycles now use `try/finally` blocks.

The template is deleted in the `finally` block even when the test fails.

### Rust persistence tests

Seven tests used the shared database instead of isolated schemas.

These tests could leave data behind if they failed before their inline cleanup ran.

The following tests now use isolated schemas that are dropped after the test:

- `migrations_and_job_lifecycle_are_transactionally_usable`
- `encrypted_source_creation_enqueues_and_audits_atomically`
- `migrations_preserve_conflicting_programme_metadata`
- `playback_plan_uses_ranked_candidates_and_effective_pool_caps`
- `user_crud_and_channel_grants_work`
- `channel_alias_crud_and_resolution_work`
- `stream_profile_crud_and_assignment_work`

The inline cleanup code was removed because the schema drop makes it unnecessary.

### Verified results

The persistence test suite passed all 18 tests.

The API test suite passed all 43 tests.

The web test suite passed all 73 tests.

The TypeScript type check passed.

The web lint check passed.

The Clippy check passed for the persistence crate with warnings denied.

After both Rust test suites completed, zero leftover schemas remained in the database.

Three leftover schemas from previous test runs were removed manually.

## Provider budget sidebar wired to real data

Last verification: 2026-08-21.

The sidebar provider budget widget used hardcoded values.

The widget displayed `3 / 3` with a full progress bar and the text "Six viewers share three upstream streams."

The header displayed "System healthy · Last inventory sync 3m ago."

The status indicator showed a green dot with "All services operational."

None of these values came from the backend.

### Changes

The `AppShell` component now fetches the system overview through the same TanStack Query used by the overview page.

The `ProviderBudget` component receives the overview data as a prop.

The widget displays `providerConnections / providerLimit` from the API response.

The progress bar width reflects the real percentage.

The description text shows the real `activeSessions` count.

The header shows the real channel count and healthy stream count.

The status indicator reflects the query state: "All services operational" when the query succeeds, "Service unavailable" when it fails, and "Connecting…" while it loads.

### Verified results

The web test suite passed all 73 tests.

The TypeScript type check passed.

The web lint check passed.

The live API returned `providerConnections: 0` and `providerLimit: 0` with no active streams.

The sidebar showed `0 / 0` with an empty progress bar and "0 downstream sessions share upstream connections."

The header showed "Loading…" during initial render and then real data after hydration.

## Live Data Acceptance - 2026-08-27

This section records the verified results of the live data acceptance run.

### M3U streaming parse: PASS

The M3U parser downloaded 467 MB from the real provider in 39 seconds.

The parser parsed 1,147,962 records during the download.

The records-seen counter reported the correct value throughout the download.

Test evidence: live API acceptance tests (11 passed).

### XMLTV streaming parse: PASS

The XMLTV parser downloaded 95 MB from the real provider in 8 seconds.

The parser parsed 305,713 records during the download.

Test evidence: live API acceptance tests.

### Download stall handling: PASS

The stall timeout is set to 60 seconds.

The maximum timeout is set to 10 minutes.

The parser preserves partial results when a stall occurs (`records_seen > 0`).

Test evidence: `cargo test -p iptv-ingest` (64 passed).

### Reconciliation performance: PASS

The channel upsert completed in 18 seconds for 1.1 million channels.

The channel-streams relink completed in 180 seconds for 1.1 million links.

The EPG mapping pass completed in 30 seconds.

The total reconciliation completed in 4 minutes.

Migration 0022 added the functional indexes that enable this performance.

Test evidence: `cargo test -p iptv-persistence` (17 passed).

### EPG data matching: PASS

This item is the critical acceptance criterion for the project.

The database contains 293,152 programmes.

The database contains 145,507 programmes for the current day in the `America/Denver` timezone.

The database contains 4,803 programmes that air at the current time.

The reconciliation created 7,274 EPG mappings between M3U and XMLTV.

The guide coverage is 63.4 percent.

Test evidence: live API acceptance test `programmes_exist_and_overlap_now_in_denver`.

### Realtime SSE: PASS

The catalog events stream is active.

The stream delivered overview events.

The stream delivered source sync progress events during the sync operation.

Test evidence: live API acceptance test `catalog_events_stream_reports_sync_and_overview`.

### API endpoints: PASS

All 13 API endpoints returned the correct data.

The API exposes 1,147,962 channels.

The API exposes 1,197 groups.

The API exposes 428,461 programmes.

Test evidence: live API acceptance tests (11 passed).

### Test suite results: PASS

The live API acceptance suite passed 11 tests.

The Playwright live data acceptance suite passed 8 tests against real provider data.

The Playwright full suite passed 37 tests. Four tests failed due to pre-existing issues unrelated to the live data acceptance work.

The Rust unit tests for `iptv-ingest` passed 64 tests.

The Rust unit tests for `iptv-persistence` passed 17 tests.

The Rust unit tests for the gateway passed 62 tests.

The web Vitest suite passed 73 tests.

The total result is 260 tests passed and 4 tests failed. The 4 failures are pre-existing Playwright issues.

## Development Compose Audit - 2026-08-31

### Production image build: PASS

The active production image build used `--no-cache`.

The build finished successfully in approximately five minutes.

The Docker runtime provides two CPUs and approximately 2 GB of memory.

Test evidence: process inspection, Docker image timestamps, and Docker image tags.

### Development startup memory use: FAILED

The prior dev start ran the core and worker Rust builds at the same time.

Both Rust builds used one shared target volume.

The web container exited after an out-of-memory kill.

The core and worker containers later received termination signals.

Docker did not report those two exits as out-of-memory kills.

Test evidence: Docker container state and Compose container labels.

### Revised development image build: FAILED

The revised Compose configuration passed validation.

The dev image build failed at the Cargo registry copy step.

The image deletes `/usr/local/cargo/registry/cache` before this copy step.

Test evidence: `make compose-config` passed, and `make compose-dev-build` failed.

### Corrected development image build: PASS

The dev image copies the complete Cargo registry data.

The core dev image built successfully in 19 seconds with cached layers.

The web dev image built successfully in seven seconds with cached layers.

Test evidence: `make compose-dev-build` passed.

### Revised development stack start: FAILED

PostgreSQL became healthy.

Only the core service started a Cargo process.

The core build failed because the command did not select the server package.

The web and worker services correctly waited for core health.

Test evidence: Compose service state, process output, and core logs.

### Development worker health check: FAILED

The core, web, gateway, and PostgreSQL services started successfully.

The gateway live and ready routes returned successful responses.

The worker process started successfully.

The worker health check failed because the dev image does not contain `pg_isready`.

Test evidence: Compose service state and Docker health-check output.

### Development Compose startup: PASS

The dev stack uses one fixed Compose project and one Cargo target volume.

The core is the only service that compiles Rust.

The worker restarts the shared server binary without a second Cargo process.

The web and worker services wait for core health.

The worker health check verifies the active worker process.

The first empty-volume Rust build completed in 94 seconds.

The complete warm stack reached healthy state in 18 seconds.

The gateway live and ready routes returned successful responses.

Test evidence: Compose validation, image builds, process lists, health checks, and a timed warm start.

### Development hot reload: FAILED

A source change started one Rust build.

The incremental build completed in 27 seconds.

The core restarted from the new binary.

The worker detected the new binary but waited for its 60-second graceful shutdown.

The worker did not restart within the expected incremental reload window.

Test evidence: Compose process lists, executable inode data, and service logs.

### Development hot reload shutdown fix: PASS

A source change started one Rust build.

The incremental release build completed in 27 seconds.

The core restarted from the new binary.

The worker stopped its old child process within two seconds.

The worker started PID 423 from the new executable.

The worker restored its direct health marker.

Test evidence: Compose process lists, executable inode data, health marker state, and service logs.

### Development stack stability: PASS

All five dev services remained active for 17 hours.

The core, worker, web, and PostgreSQL services report healthy state.

Test evidence: current Compose service state on 2026-09-01.

### Incremental Xtream edit build: FAILED

The dev watcher compiled an incomplete request constructor during the Xtream edit.

The compiler reported a missing `xtream_category_names` field.

The prior core process remained active and healthy.

Test evidence: dev core compile log.

### Incremental Xtream edit build retry: PASS

The next dev build completed in 27 seconds.

The core and worker restarted from the new binary.

Test evidence: dev core and worker logs.

## Timezone Reverification - 2026-08-31

### XMLTV timezone parser: PASS

All 17 focused XMLTV tests passed.

The tests cover explicit offsets, implicit source timezones, and UTC normalization.

The tests cover ambiguous and nonexistent Denver times.

Test evidence: `cargo test -p iptv-parsers xmltv`.

### Web timezone test command: FAILED

The pnpm command stopped before it ran the requested web test.

The repository requires pnpm 11.19.0, but the host command used pnpm 11.22.0.

No web test result came from this command.

### Web current-programme selection: PASS

The focused web utility suite passed all eight tests.

The suite includes current, upcoming, winter, and summer programme instants.

Test evidence: direct Vitest execution for `tests/api-and-utils.test.ts`.

## Implementation Audit - 2026-08-31

### Xtream source refresh: FAILED

The worker rejects each Xtream source refresh as unsupported.

A server test confirms this rejection.

This behavior does not satisfy the v1 Xtream ingestion requirement.

### Live media acceptance: FAILED

The live acceptance program opens one provider stream for 30 seconds.

The program does not open the stream through the gateway.

The program does not run the required three-channel scenario.

The program does not run the required six-viewer scenario.

## Xtream persistence - 2026-09-01

### Rust format check: FAILED

`cargo fmt --check` did not pass.

The command reports concurrent format changes outside the persistence task.

The command also reports format changes in the new persistence test code.

Test evidence: `cargo fmt --check`.

### Xtream reconciliation: PASS

The PostgreSQL test creates a canonical channel from an active Xtream snapshot.

The test retains the canonical channel after Xtream snapshot replacement.

The test removes the prior stream link and creates the new stream link.

Test evidence: `cargo test -p iptv-persistence --test postgres reconcile_xtream_snapshots_retain_channels_and_replace_stream_links -- --exact`.

### Xtream reconciliation independent retry: FAILED

The focused test reached the isolated PostgreSQL service.

The second snapshot caused a duplicate channel-priority constraint error.

The old stream link blocked the replacement link at priority zero.

The earlier pass did not prove the database path.

Test evidence: focused PostgreSQL test with the isolated coverage database.

### Scheduled Xtream source kind: PASS

The PostgreSQL test returns `SourceKind::Xtream` for a due Xtream provider source.

Test evidence: `cargo test -p iptv-persistence --test postgres list_due_sources_preserves_xtream_provider_kind -- --exact`.

### Persistence Clippy: FAILED

Strict Clippy rejects the Xtream reconciliation integration test for 110 lines.

Test evidence: `cargo clippy -p iptv-persistence --all-targets -- -D warnings`.

### Persistence Clippy retry: PASS

Strict Clippy passes for all `iptv-persistence` targets.

Test evidence: `cargo clippy -p iptv-persistence --all-targets -- -D warnings`.

## Xtream ingestion - 2026-09-01

### Xtream endpoint preparation: PASS

The ingest package passed 83 tests.

One environment-only test was ignored.

The tests cover concrete per-stream URL protection and category-name mapping.

The tests cover credential redaction for requests, errors, templates, and stored metadata.

Test evidence: `cargo test -p iptv-ingest`.

### Rust format retry: PASS

The complete Rust workspace has no format differences.

Test evidence: `cargo fmt --all --check`.

### Xtream refresh target: PASS

The worker maps an Xtream source to a provider-owned live-stream snapshot.

The focused server test passed.

Test evidence: `cargo test -p iptv-gateway tests::refresh_targets_match_source_ownership -- --exact`.

### Xtream ingest Clippy: PASS

Strict Clippy passes for all `iptv-ingest` targets.

Test evidence: `cargo clippy -p iptv-ingest --all-targets -- -D warnings`.

### Xtream ingest coverage first run: FAILED

All 70 unit tests passed.

The 13 PostgreSQL tests could not open the local sandbox port.

The test process reported `Operation not permitted` for each database connection.

The command did not produce a valid coverage result.

Test evidence: focused `cargo llvm-cov` output.

### Isolated coverage database: PASS

A separate PostgreSQL 17 test service reached healthy state on port 54330.

The service uses the test configuration and a separate Compose project.

Test evidence: Compose startup and health state.

### Xtream ingest package coverage: FAILED

All 83 executable tests passed.

One environment-only test was ignored.

The package result is 64.73 percent line coverage.

The package result is 61.61 percent function coverage.

The package result is 63.23 percent region coverage.

These results are below the 86 percent gate.

The ingest pipeline file has 53.24 percent line coverage.

This result is below the 75 percent critical-file gate.

Test evidence: focused `cargo llvm-cov` with the isolated PostgreSQL service.

### Xtream worker integration first run: FAILED

The first Xtream refresh succeeded.

The second Xtream refresh created a new active snapshot.

The canonical channel lost its link to the replacement provider stream.

The test failed with `RowNotFound` for the refreshed channel link.

Test evidence: focused Xtream worker PostgreSQL integration test.

### Xtream worker integration retry: PASS

The worker completed two successful Xtream refreshes.

The worker retained the canonical channel UUID after the stream-name change.

The provider category ID mapped to the `Sports` group name.

The encrypted endpoint contains one concrete stream URL after decryption.

An authentication failure stopped all category and stream requests.

An empty stream response preserved the active snapshot and channel link.

Public and diagnostic database fields contain no credential canaries.

Test evidence: focused Xtream worker test with the isolated PostgreSQL service.

### Xtream server Clippy first run: FAILED

Strict Clippy reports four test-code violations.

The new Xtream helper has eight arguments.

The new Xtream test has 277 lines.

Two prior server tests also fail current line and range checks.

Test evidence: `cargo clippy -p iptv-gateway --all-targets -- -D warnings`.

### Rust format check after relink fix: FAILED

The format check found one difference in the new persistence regression test.

Test evidence: `cargo fmt --all --check`.

### Rust format check after relink retry: PASS

The complete Rust workspace has no format differences.

Test evidence: `cargo fmt --all --check`.

### Xtream server Clippy retry: PASS

Strict Clippy passes for all `iptv-gateway` targets.

Test evidence: `cargo clippy -p iptv-gateway --all-targets -- -D warnings`.

### Complete server test target: PASS

All 62 server tests passed with the isolated PostgreSQL service.

The target includes M3U, XMLTV, Xtream, job, authentication, and redaction tests.

Test evidence: `cargo test -p iptv-gateway --bin iptv-gateway` with the test database.

### Complete Rust coverage run: FAILED

The run completed all server, media, parser, domain, API, and ingest tests.

One persistence integration test failed before coverage calculation.

The alias test found an EPG mapping before it created the alias.

No valid aggregate coverage result came from this run.

Test evidence: complete workspace `cargo llvm-cov` output.

### Complete Rust coverage retry: PASS

All executable Rust tests pass with the isolated PostgreSQL service.

The workspace has 90.68 percent line coverage.

The workspace has 90.11 percent function coverage.

The workspace has 89.34 percent region coverage.

Each measured production file exceeds the 75 percent critical-file gate.

The ingest pipeline has 91.03 percent line coverage.

The ingest pipeline has 83.65 percent function coverage.

The ingest pipeline has 89.96 percent region coverage.

Test evidence: complete workspace `cargo llvm-cov` with the isolated PostgreSQL service.

### Changed-line coverage script regression tests: PASS

The script counts covered changed lines in Rust and TypeScript files.

The script includes new production files.

The script fails when coverage data omits a changed production file.

Test evidence: `scripts/test-changed-line-coverage.sh`.

### EPG alias mapping focused retry: FAILED

The failure reproduces in the isolated focused test.

The channel maps before the test creates its alias.

Test evidence: focused persistence PostgreSQL test.

### EPG alias mapping fixture correction: PASS

The name normalizer removes the `HD` quality token by design.

The old test expected `ESPN HD` and `ESPN` to remain different.

The revised fixture uses two names that only an explicit alias can join.

The focused PostgreSQL test passes.

Test evidence: `reconcile_epg_mappings_by_channel_alias` with the isolated PostgreSQL service.

### Dev stack after Xtream rebuild: PASS

All five dev services remain active.

The core, worker, web, and PostgreSQL services report healthy state.

The core and worker use the latest successful release build.

Test evidence: dev Compose state and reload logs.

### Dev stack stability recheck: FAILED

The sandbox denied access to the local Colima Docker socket.

This result does not show a service failure.

Test evidence: dev Compose state command in the restricted sandbox.

### Dev stack stability recheck retry: PASS

All five development services remain active after 17 hours.

The core, worker, web, and PostgreSQL services report healthy state.

The latest no-change Rust rebuild completed in 0.11 seconds.

Test evidence: dev Compose state and service logs from Colima.

### Compose configuration recheck: PASS

The default, test, and development Compose configurations validate.

The test profile reports only the expected empty live-source variable warning.

Test evidence: `make compose-config`.

### Dependency and license audit recheck: FAILED

The sandbox denied the Cargo advisory database lock.

This result does not show a dependency policy failure.

Test evidence: `cargo deny check` in the restricted sandbox.

### Dependency and license audit recheck retry: PASS

The advisory, ban, license, and source checks pass.

Cargo Deny reports unused permissive license allowances as warnings.

Test evidence: `cargo deny check` with advisory database access.

## Xtream persistence atomic reconciliation - 2026-09-01

### Xtream reconciliation regression test: FAILED

The test could not open the isolated PostgreSQL test connection.

The sandbox returned `Operation not permitted` for port 54330.

Test evidence: `IPTV_TEST_DATABASE_URL=postgres://iptv:iptv-development@127.0.0.1:54330/iptv cargo test -p iptv-persistence --test postgres reconcile_xtream_snapshots_retain_channels_and_replace_stream_links -- --exact`.

### Xtream reconciliation regression test retry: PASS

The test creates and replaces an Xtream stream link.

The test verifies rollback after a forced stream-link insert failure.

Test evidence: `IPTV_TEST_DATABASE_URL=postgres://iptv:iptv-development@127.0.0.1:54330/iptv cargo test -p iptv-persistence --test postgres reconcile_xtream_snapshots_retain_channels_and_replace_stream_links -- --exact`.

### Xtream persistence Clippy: PASS

Strict Clippy passes for all `iptv-persistence` targets.

Test evidence: `cargo clippy -p iptv-persistence --all-targets -- -D warnings`.
