# IPTV Gateway Implementation Plan

Updated: 2026-08-20

## Purpose

This file defines the next implementation work for IPTV Gateway v1.

Use `IMPLEMENTATION_STATUS.md` as the test record.

Do not mark work complete without current test evidence.

Do not put credentials, secret URLs, or tokens in either file.

## Work Rules

1. Select the first unassigned item in the highest priority section.
2. Record the item owner before you change shared code.
3. Preserve work from other agents.
4. Add tests with each production change.
5. Run the focused tests before you run broad tests.
6. Update `IMPLEMENTATION_STATUS.md` after each verified test or confirmed failure.
7. Update this file when an item changes state or priority.
8. Use ASD-STE100 Issue 9 for all technical documentation.

## Current Gate State

The web coverage gate passes.

The M3U and XMLTV parser coverage gates pass.

The ingest parser and pipeline coverage gates pass.

The mandatory 120-second media acceptance test passes both viewer scenarios.

The API library line and function results exceed 82 percent.

The complete Rust aggregate coverage gate passes.

The core and four workers use one current Rust image.

The core and four workers stayed healthy for five minutes.

The native HTTP MPEG-TS path owns the active shared sessions.

The shared session manager owns native, FFmpeg, and VLC sessions.

The credential broker supports direct MPEG-TS and bounded HLS resources.

## Priority 0: Restore Required Gates

### P0.1 Restore the Compose Workers

State: Complete

Owner: Root

1. Rebuild the `core` and `worker` images from the current workspace.
2. Recreate the `core` and `worker` containers.
3. Confirm that all migrations resolve in each process.
4. Confirm that each worker stays active for five minutes.
5. Confirm that each worker claims jobs with a unique worker ID.
6. Run the base Compose health checks.
7. Record the result in `IMPLEMENTATION_STATUS.md`.

Completion evidence:

- The `core`, `web`, `postgres`, and all worker containers stay healthy.
- No process reports a missing migration.
- A durable test job reaches its terminal state.

Current progress:

- Compose builds one shared Rust image.
- The core and four workers use the shared image.
- The core and four workers stayed healthy for five minutes.
- No recreated process reported a missing migration.
- Four distinct hostnames provide four default worker IDs.
- A durable no-op job reached `succeeded` after one attempt.
- The complete base stack passed on alternate host port 18080.
- The gateway core health route passed.
- The gateway web route passed.

Verified result:

- The core, web, PostgreSQL, and four workers report healthy.
- The four workers use unique default worker IDs.
- A durable no-op job reached `succeeded`.
- The gateway passed on a configurable host port.

### P0.2 Restore the Rust Coverage Gate

State: Active

Owner: Root and changed-line gate agent

1. Complete the active API and server coverage work.
2. Run the complete Rust coverage command.
3. Use the approved exclusions only for test-provider and load-generator binaries.
4. Add tests for the largest uncovered production branches.
5. Do not exclude parser, mapper, event, slot, session, buffer, recovery, authentication, or redaction code.
6. Repeat the complete coverage command after each focused improvement.

Completion evidence:

- Rust line coverage is at least 86 percent.
- Rust function coverage is at least 86 percent.
- Rust region coverage is at least 86 percent.
- Each critical production file has at least 75 percent coverage.
- Changed production lines have at least 95 percent coverage.

Current progress:

- The API library has 82.77 percent line coverage.
- The API library has 83.91 percent function coverage.
- The API library has 80.53 percent region coverage.
- All 43 API tests passed in the latest complete Rust run.
- The generated-guide collision regression is resolved.
- The focused PostgreSQL collision regression now passes.
- Server line coverage is 87.19 percent.
- Server function coverage is 89.53 percent.
- Server region coverage is 87.79 percent.
- Complete Rust line coverage is 92.12 percent.
- Complete Rust function coverage is 91.77 percent.
- Complete Rust region coverage is 90.54 percent.
- The complete run passed all 61 gateway tests.
- The complete run passed all 43 API tests.
- The ingest library facade has 100 percent line coverage.
- The ingest library facade has 100 percent function coverage.
- The ingest library facade has 100 percent region coverage.
- Each reported critical production file exceeds 75 percent line coverage.
- The first changed-line run reported 76.36 percent.
- That run sent TypeScript files to Rust coverage.
- The changed-line gate needs separate Rust and TypeScript accounting.
- Separate Rust and TypeScript accounting now works.
- The truthful changed-line result is 89.91 percent.
- The 95 percent changed-line requirement remains open.

### P0.3 Make Process Shutdown Safe

State: Complete

Owner: Process session safety agent

1. Keep the provider slot until the child process stops.
2. Keep the provider slot until the credential broker stops.
3. Reap each child process after the last viewer leaves.
4. Add race tests for final-viewer shutdown.
5. Add tests for child exit and broker exit.
6. Run the strict media Clippy check.

Completion evidence:

- No test releases a slot before the process and broker stop.
- No child process remains after the final viewer leaves.
- All media tests pass.

Verified result:

- All 49 media tests pass.
- The strict media Clippy check passes.
- The provider slot remains active through child reap and broker shutdown.

### P0.4 Run the Complete Local Gate

State: Waiting for P0.1 and P0.2

Owner: Root

If all focused suites pass, run the complete local gate.

Run these checks:

- Rust formatting.
- Rust tests with PostgreSQL.
- Rust Clippy with warnings denied.
- Rust coverage.
- Changed-line coverage.
- TypeScript type checks.
- TypeScript lint.
- TypeScript tests and coverage.
- Playwright tests.
- Dependency and license audit.
- Base and test Compose validation.
- Documentation link and format checks.

Record each pass or failure in `IMPLEMENTATION_STATUS.md`.

## Priority 1: Complete the Media Plane

### P1.1 Activate FFmpeg and VLC for Shared Sessions

State: Complete

Owner: Root and API coverage agent

1. Map the database adapter policy to each source specification.
2. Apply the configured `auto`, `native-ts`, `ffmpeg`, or `vlc` policy.
3. Keep one logical provider lease during reconnect and failover.
4. Keep provider credentials out of child arguments.
5. Publish typed adapter diagnostics without secret data.
6. Add deterministic FFmpeg and VLC acceptance tests.

Completion evidence:

- Two viewers share one process and one provider slot.
- A ring wrap does not stop the child process.
- The final viewer stops the process within two seconds.
- Redaction tests find no provider credential in arguments or diagnostics.

Current progress:

- All 52 media tests pass.
- FFmpeg and VLC share one process and one provider slot for two viewers.
- FFmpeg reaches `Streaming` only after a confirmed PAT and PMT boundary.
- The final viewer stops the FFmpeg session within two seconds.
- The control API applies the stored adapter policy.
- Invalid stored adapter values return a redacted typed error.

### P1.2 Add HLS Credential Brokerage

State: Complete

Owner: Process session safety agent

1. Proxy the HLS manifest through a loopback broker.
2. Rewrite each segment URL to a short-lived local URL.
3. Support required provider headers for each request.
4. Bound the token lifetime and request count.
5. Redact upstream URLs and headers.
6. Add malformed-manifest and expired-token tests.

Completion evidence:

- FFmpeg receives only loopback URLs.
- The broker serves a manifest and all required segments.
- An expired token cannot fetch a segment.
- Logs and process arguments contain no provider credential.

Current progress:

- The HLS broker rewrites manifests and nested playlists.
- The HLS broker proxies segments, AES keys, and maps.
- The HLS broker enforces time, request, token, URI, and response limits.
- The process start path accepts an explicit HLS format.
- The FFmpeg manager test passes nested HLS resources through one shared process.
- The credential broker has 93.72 percent line coverage.
- The credential broker has 91.95 percent function coverage.
- The credential broker has 92.37 percent region coverage.

### P1.3 Complete Recovery and Failover

State: Partial

Owner: Unassigned

1. Coordinate reconnects once per canonical channel.
2. Keep downstream connections open during the recovery window.
3. Emit bounded MPEG-TS keepalive data after the ring empties.
4. Resume at a confirmed PAT and PMT boundary.
5. Rank alternate streams with current health data.
6. Keep the same provider lease during failover.

Completion evidence:

- One channel failure does not affect another channel.
- Six viewers do not cause duplicate reconnects.
- Recovery does not exceed the provider cap.
- A failed recovery closes viewers with a typed error.

## Priority 2: Complete Catalog and Guide Behavior

### P2.1 Generate Dynamic Event Programmes

State: Partial

Owner: Unassigned

The event scan stores matched channels and durable generated programmes.

Current progress:

- The parser builds deterministic multi-event schedules.
- The parser prevents overlap and adds contiguous filler.
- The parser accepts captured durations and IANA timezones.
- The parser rejects ambiguous and nonexistent DST times.
- The XMLTV output publishes durable event and filler intervals.
- The event parser coverage passes every critical-file threshold.
- The durable rescan bridge and PostgreSQL lifecycle test pass.
- The worker invokes the dynamic scan after M3U reconciliation.
- The worker skips the dynamic scan after a failure or XMLTV refresh.
- The storage model stores event, filler, stable-key, title, and provenance data.
- A conflicting template preserves the existing valid guide.
- Clean storage line coverage is 95.93 percent.
- Clean storage function coverage is 94.96 percent.
- Clean storage region coverage is 92.27 percent.
- The scan uses legacy templates and built-in sports rules.
- Stored rule sets do not drive the scan.
- Per-template filler and category settings do not drive generation.
- Generated programmes do not have a control API view.

1. Parse event date, time, timezone, teams, league, and duration.
2. Reject ambiguous or nonexistent local times.
3. Build a stable event key.
4. Generate one event programme at its UTC interval.
5. Generate contiguous filler before and after each event.
6. Prevent overlaps on one channel.
7. Preserve the literal provider title and provenance.
8. Add group templates and preview tests.

Completion evidence:

- Built-in `v`, `vs`, `versus`, and `@` patterns pass.
- ISO timestamp patterns pass.
- DST gap and overlap tests pass.
- XMLTV output contains the event and filler intervals.
- A second scan updates the same stable event.

### P2.2 Correct Guide Timezones and Current Programme Selection

State: Complete

Owner: Dynamic programmes agent

1. Parse explicit XMLTV offsets and `Z` timestamps.
2. Apply the source timezone only to timestamps without offsets.
3. Store all programme intervals in UTC.
4. Preserve each original timestamp and source timezone.
5. Select current programmes with UTC half-open intervals.
6. Serialize XMLTV timestamps with explicit offsets.
7. Render guide times in the operator timezone.
8. Add winter, summer, and DST boundary tests.

Completion evidence:

- A current UTC programme appears in the current guide slot.
- A Denver winter programme maps to UTC minus seven hours.
- A Denver summer programme maps to UTC minus six hours.
- Explicit XMLTV offsets override the source timezone.
- Ambiguous and nonexistent local times produce diagnostics.
- The output retains the correct programme interval.

Verified result:

- The XMLTV timezone suite passed all 17 tests.
- UTC and explicit offset tests pass.
- Denver winter and summer tests pass.
- Ambiguous and nonexistent DST tests pass.
- Literal XMLTV timestamps persist with normalized UTC instants.
- A PostgreSQL query returns current, future, and past programmes in order.
- The API suite passed all 43 tests.
- The focused web suite passed all 33 tests.
- The TV Guide uses the browser timezone for display.

### P2.3 Complete Mapping Review and Rollback

State: Partial

Owner: Unassigned

1. Store every automatic match score and reason.
2. Send ambiguous matches to the review queue.
3. Preserve each manual binding.
4. Prevent an empty result from replacing a valid lineup.
5. Add preview, confirmation, revision, and rollback operations.
6. Add UI tests for bulk review and rollback.

Completion evidence:

- Mapping order follows the approved precedence.
- Permutation tests produce the same result.
- A zero-result update preserves the active generation.
- A rollback restores channels, streams, and EPG mappings.

### P2.4 Meet the Scale Gates

State: Not verified on the required runner

Owner: Unassigned

1. Generate the 1.16-million-entry M3U fixture.
2. Generate the 275,000-programme XMLTV fixture.
3. Measure parse time and peak memory.
4. Measure staging and activation time.
5. Reject an allocation or time regression above ten percent.

Completion evidence:

- M3U parsing takes no more than 15 seconds.
- M3U parsing uses no more than 512 MiB RSS.
- M3U activation takes no more than 90 seconds.
- XMLTV parsing takes no more than 10 seconds.
- XMLTV parsing uses no more than 512 MiB RSS.
- XMLTV activation takes no more than 45 seconds.

## Priority 3: Complete Security and Operations

### P3.1 Add OIDC

State: Not implemented

Owner: Unassigned

1. Add the authorization-code flow with PKCE.
2. Validate the configured issuer and client.
3. Apply the approved subject or email allowlist.
4. Retain the local administrator as break-glass access.
5. Add callback, state, nonce, replay, and logout tests.

### P3.2 Add Operator API Tokens

State: Not implemented

Owner: Unassigned

The bootstrap bearer disables after the first successful password sign-in.

1. Store only operator token hashes.
2. Add token creation, rotation, revocation, and audit events.
3. Show plaintext only once after creation.
4. Add scope checks for control API access.
5. Add expiration and replay tests.

### P3.3 Complete Revisions and Effective Configuration

State: Partial

Owner: Unassigned

1. Publish backend configuration descriptions and defaults.
2. Return each effective value and inheritance source.
3. State whether each change needs a restart or reimport.
4. Require ETag checks for conflicting edits.
5. Add revision history and rollback tests.

### P3.4 Add Real Stream Health Jobs

State: Partial

Owner: Unassigned

The schema and API store stream health data.

The worker does not run the complete probe policy.

1. Acquire a low-priority provider slot before each probe.
2. Skip the probe when provider capacity is full.
3. Validate PAT, PMT, audio, video, and sustained packet flow.
4. Store redacted typed failures and quality data.
5. Feed health data into alternate-stream rank.

## Priority 4: Complete Product Workflows

### P4.1 Complete the Management UI

State: Partial

Owner: Unassigned

1. Complete source progress, cancellation, and retry workflows.
2. Complete mapping review and rollback workflows.
3. Complete event rule previews.
4. Complete provider pool and adapter policy forms.
5. Complete active viewer and upstream diagnostics.
6. Complete support bundle and redacted log workflows.
7. Add accessibility tests for each workflow.

### P4.2 Complete Jellyfin Setup Verification

State: Partial

Owner: Unassigned

1. Verify M3U and XMLTV import in a real Jellyfin container.
2. Verify HDHomeRun discovery data and lineup data.
3. Verify the configured tuner count.
4. Verify output-token rotation and overlap.
5. Verify profile channel subsets.
6. Add the optional SSDP Compose profile.

### P4.3 Complete Documentation

State: Partial

Owner: Unassigned

1. Keep all documentation aligned with verified behavior.
2. Apply ASD-STE100 Issue 9 to each technical page.
3. Add source, Jellyfin, security, backup, and recovery procedures.
4. Add a generated OpenAPI 3.1 contract.
5. Add contract-drift checks for the TypeScript client.
6. Install MkDocs in the documentation test environment.
7. Run the strict documentation build.

## Deferred Work

Do not implement these items before the v1 local gates pass:

- GitHub Actions image publication.
- Multi-architecture image publication.
- Software bills of materials.
- Build provenance and image signatures.
- Active-active media clusters.
- DVR, timeshift, catch-up, and VOD.
- DRM support.
- Video or audio transcoding.
- Third-party plugin execution.

## Required Final Verification

When all v1 items are complete, run these acceptance tests:

1. Run the three-channel test for 120 seconds.
2. Run the six-viewer test for 120 seconds.
3. Run the nightly media test for 30 minutes.
4. Run the fault test with latency, stalls, resets, and child exits.
5. Run the live-provider test with environment-injected credentials.
6. Run the complete Rust and TypeScript coverage gates.
7. Run the dependency, license, security, and documentation gates.
8. Run a clean Docker Compose installation.

Do not publish a v1 release until every required gate passes.
