# IPTV Gateway Implementation Status

Last update: 2026-08-19

## Ingest store coverage

The `crates/ingest/src/store.rs` line coverage is 93.35%.

The region coverage is 99.02%.

Thirteen PostgreSQL integration tests cover the `PgSnapshotStore::activate` method and its helper functions.

The tests cover empty snapshot rejection, owner validation, M3U activation, XMLTV activation, Xtream activation, snapshot supersession, duplicate checksum reuse, batch boundaries for provider streams and programmes, and error paths for mismatched owners.

Clippy passes for `iptv-ingest` with warnings denied.

Formatting passes for the workspace.

This file records verified results. It does not contain credentials or secret URLs.

## Status definitions

- **Working**: A test passed in the current workspace.
- **Failed**: A test or runtime operation failed.
- **Blocked**: A required dependency or decision prevents a test.
- **Todo**: The implementation or test is not complete.

## Working

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

The API has an in-memory fallback for tests or configurations without persistence.

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

## Working

### Server main.rs coverage

The `apps/server/src/main.rs` line coverage is 85.01%.

The region coverage is 84.92%.

Seven new tests cover the `process_job` function, the `run_source_refresh` error paths, the `WorkerJobControl::checkpoint` implementation, and the `RefreshError::persisted_summary` variants.

The database-backed tests use the PostgreSQL test database and clean up all inserted rows.

Clippy passes for `iptv-gateway` with warnings denied.

Formatting passes for the workspace.

## Blocked

No item is blocked.

## Todo

- Add the changed-production-line coverage gate.
- Run the 120-second deterministic media acceptance test.
- Run the fault-injection acceptance tests.
- Run the live-provider media acceptance test.
- Complete the remaining implementation plan.
