# IPTV Gateway Implementation Plan

Updated: 2026-09-01

## Purpose

This file defines the remaining work for IPTV Gateway v1.

Use `IMPLEMENTATION_STATUS.md` as the test record.

Do not mark an item complete without current test evidence.

Do not put credentials, secret URLs, or tokens in either file.

## Work Rules

1. Select the first unassigned item in the highest priority section.
2. Record the owner before you change shared code.
3. Preserve all unrelated changes from other workers.
4. Add tests with each production change.
5. Keep Rust and TypeScript coverage at 86 percent or more.
6. Keep changed production line coverage at 95 percent or more.
7. Run focused tests before broad tests.
8. Update `IMPLEMENTATION_STATUS.md` after each verified test or confirmed failure.
9. Update this file when an item changes state or priority.
10. Use ASD-STE100 Issue 9 for all technical documentation.
11. Do not add DVR, VOD, catch-up, timeshift, or transcoding work to v1.

## Verified Baseline

The live M3U parser processed 1,147,962 records in 39 seconds.

The live XMLTV parser processed 305,713 records in eight seconds.

The Denver guide query returned 4,803 programmes for the current time.

The deterministic media suite previously passed the three-channel and six-viewer scenarios.

The current timezone path applies source timezones only to timestamps without offsets.

The current timezone path stores UTC intervals and uses half-open current-programme selection.

The dev Compose stack reached healthy state in 18 seconds after a warm start.

The first dev Rust build used one compiler and completed in 94 seconds.

The complete Rust coverage result is 90.68 percent for lines.

The complete Rust coverage result is 90.11 percent for functions.

The complete Rust coverage result is 89.34 percent for regions.

The last changed-line coverage result is 89.91 percent.

Four Playwright tests failed in the last complete browser run.

The Xtream worker path is under integration test.

The live acceptance program opens one provider stream without the gateway.

## Priority 0: Restore Required Product Gates

### P0.1 Stabilize Development Compose

State: Complete

Owner: Root

The dev stack uses one fixed project and one shared Cargo target volume.

The core service owns all Rust builds.

The worker restarts the shared binary without a Cargo process.

The web and worker services wait for core health.

Completion evidence:

- `make compose-config` passes.
- `make compose-dev-build` passes.
- All five dev services report healthy.
- The gateway live and ready routes pass.
- A warm start reaches healthy state in 18 seconds.

### P0.2 Implement Xtream Live Runtime Ingestion

State: Runtime and workspace coverage verified; changed-line coverage required

Owner: Root

The parser and storage layers support Xtream live streams.

The worker now connects authentication, categories, live streams, and catalog reconciliation.

Current evidence:

- Credential-safe endpoint tests pass.
- Xtream ingest tests pass.
- Xtream persistence and reconciliation tests pass.
- The worker integration test passes.
- Failed refreshes preserve the active snapshot and channel link.
- The complete Rust coverage gate passes.
- Each measured production file exceeds the critical-file gate.
- The changed-line coverage gate requires a new result.

Work:

1. Treat the saved player API URL as secret data.
2. Derive the auth, category, and live-stream requests without exposing credentials.
3. Validate the authentication response before other requests.
4. Download each JSON response with the configured artifact limits.
5. Map category identifiers to stable category names.
6. Build protected live stream endpoints for each stream identifier.
7. Activate the Xtream snapshot through the existing transaction path.
8. Reconcile channels, EPG mappings, and dynamic events after activation.
9. Preserve the prior snapshot after authentication, parse, or empty-result failure.
10. Add a deterministic fake Xtream server.
11. Add worker, redaction, parser, and PostgreSQL integration tests.
12. Correct all documentation claims after the tests pass.

Completion evidence:

- A refresh job reaches `succeeded` for a fake Xtream account.
- The active snapshot contains the expected live streams.
- The channel group uses the category name.
- No stored diagnostic or log contains a credential.
- A second refresh preserves stable channel identities.
- An invalid or empty response preserves the prior active snapshot.
- All focused coverage gates pass.

### P0.3 Implement Xtream Short EPG Jobs

State: Pending

Owner: Unassigned

Start this item after P0.2 passes.

Work:

1. Schedule bounded short EPG requests for selected live streams.
2. Respect account and shared-pool request policies.
3. Normalize each programme interval to UTC.
4. Preserve provider timestamps and provenance.
5. Activate the guide only after all sanity checks pass.
6. Preserve the prior guide after an empty or failed refresh.
7. Add partial-failure, limit, timezone, and cancellation tests.

Completion evidence:

- The guide shows a current Xtream programme.
- Explicit offsets override the configured source timezone.
- Requests stay within configured bounds.
- Failed short EPG requests do not clear a working guide.

### P0.4 Restore Coverage Gates

State: Active

Owner: Root

The current aggregate Rust results exceed all three 86 percent gates.

Each measured production file exceeds the 75 percent critical-file gate.

The focused ingest-only result remains below the aggregate gate.

The complete suite gives the ingest pipeline 91.03 percent line coverage.

Work:

1. Keep the complete Rust coverage result current.
2. Run the complete TypeScript coverage command.
3. Run the changed-line coverage command.
4. Add tests for each uncovered production branch.
5. Keep each critical production file at 75 percent or more.
6. Use only the approved coverage exclusions.

Completion evidence:

- Rust line, function, and region coverage reach 86 percent.
- TypeScript line, function, and branch coverage reach 86 percent.
- Changed production line coverage reaches 95 percent.
- Each critical production file reaches 75 percent.

### P0.5 Repair Browser Acceptance and Test Reports

State: Active

Owner: Playwright repair agent

The last complete Playwright run has four failures.

Current reports incorrectly label this result as a pass.

Work:

1. Reproduce all four failures against the current dev stack.
2. Classify each failure as product, fixture, or test failure.
3. Fix each confirmed failure.
4. Remove false pass statements from test reports.
5. Add the complete Playwright suite to the local gate.

Completion evidence:

- The complete Playwright suite passes.
- The test report matches the command results.
- `IMPLEMENTATION_STATUS.md` records each result accurately.

### P0.6 Build Real Gateway Live Acceptance

State: Pending

Owner: Unassigned

The current live program tests one direct provider connection for 30 seconds.

Work:

1. Select three stable channels with credential-independent identities.
2. Probe candidates sequentially within the provider cap.
3. Reset counters after candidate selection.
4. Open three viewers through gateway output routes.
5. Keep all three viewers active for 120 seconds.
6. Open six viewers with two viewers per channel.
7. Keep all six viewers active for 120 seconds.
8. Assert exactly three upstream sessions.
9. Assert that the high-water provider count equals three.
10. Assert that no viewer drops after warmup.
11. Add a configurable 30-minute soak mode.

Completion evidence:

- Both 120-second scenarios pass through the gateway.
- Two viewers of one channel share one upstream session.
- The provider connection count never exceeds three.
- The final viewer closes its upstream and releases its slot.

### P0.7 Complete the Local Gate Command

State: Pending

Owner: Unassigned

The current `make ci` command omits required suites.

Work:

1. Add Playwright to the local gate.
2. Add the fuzz smoke tests.
3. Add deterministic media acceptance.
4. Add fault acceptance.
5. Add Compose health verification.
6. Add the strict documentation build.
7. Keep the live provider test as an explicit credentialed gate.
8. Stop the command immediately after a failed required gate.

Completion evidence:

- One local command runs every non-secret required gate.
- The command returns a failure status after any required failure.
- The status file records the complete command result.

## Priority 1: Complete the Media Plane

### P1.1 Reverify Recovery and Failover

State: Recheck required

Owner: Unassigned

The session code contains recovery tests that did not exist in the old plan.

Work:

1. Run all current recovery and failover tests.
2. Map each test to the approved recovery requirements.
3. Keep downstream connections open for the bounded recovery window.
4. Emit bounded MPEG-TS keepalive data after the ring empties.
5. Resume only at a PAT and PMT boundary.
6. Coordinate one reconnect for all channel viewers.
7. Keep one logical provider lease during failover.
8. Add tests for each uncovered fault.

Completion evidence:

- One channel failure does not affect another channel.
- Six viewers cause one reconnect for their channel.
- Recovery never exceeds the provider cap.
- A failed recovery closes viewers with a typed error.

### P1.2 Reverify FFmpeg, VLC, and HLS Brokerage

State: Recheck required

Owner: Unassigned

The shared managers and credential broker have extensive focused tests.

Work:

1. Run the native, FFmpeg, VLC, and HLS focused suites.
2. Verify process and broker shutdown after the final viewer.
3. Verify credential redaction in arguments and diagnostics.
4. Run repeated ring-wrap tests for each process adapter.

Completion evidence:

- Two viewers share one process and one provider slot.
- Ring wrap does not restart an adapter.
- Final-viewer shutdown completes within two seconds.
- Child arguments contain no provider credential.

## Priority 2: Complete Catalog and Guide Behavior

### P2.1 Complete Dynamic Event Programmes

State: Partial

Owner: Unassigned

Generated event programmes reach XMLTV output.

The TV Guide control API does not return generated programmes.

Stored event rule sets do not control the scan.

Work:

1. Connect stored rule sets to the event scan.
2. Apply group timezone, duration, filler, and title settings.
3. Return generated programmes through the control API.
4. Show generated programmes in the TV Guide.
5. Preserve the source title and provenance.
6. Prevent overlaps after repeated scans.
7. Add API, UI, database, and XMLTV tests.

Completion evidence:

- The same event appears in the API and XMLTV output.
- The TV Guide shows the event at its current local time.
- A second scan updates the same stable event.
- Filler remains contiguous and does not overlap the event.

### P2.2 Preserve Timezone Correctness

State: Complete with regression coverage required

Owner: Unassigned

The 2026-08-31 audit found no current timezone defect.

Work:

1. Keep explicit XMLTV offsets authoritative.
2. Apply the source timezone only when the source omits an offset.
3. Store programme intervals in UTC.
4. Use half-open intervals for current programme selection.
5. Add one real UI regression for the operator timezone.

Completion evidence:

- A current Denver programme appears in the current guide slot.
- Winter and summer offsets remain correct.
- DST ambiguity produces a diagnostic.
- XMLTV output contains explicit offsets.

### P2.3 Complete Mapping Review and Rollback

State: Partial

Owner: Unassigned

Work:

1. Store every automatic score and reason.
2. Send ambiguous candidates to the review queue.
3. Preserve each manual binding.
4. Prevent empty results from replacing a valid lineup.
5. Add preview, confirmation, revision, and rollback operations.
6. Add bulk review and rollback UI tests.

Completion evidence:

- Mapping order follows the approved precedence.
- Input permutations produce the same result.
- A zero-result update preserves the active generation.
- A rollback restores channels, streams, and EPG mappings.

### P2.4 Verify Scale Gates

State: Partial

Owner: Unassigned

Live parse results exist, but the required pinned-runner gates remain incomplete.

Work:

1. Generate the 1.16-million-entry M3U fixture.
2. Generate the 275,000-programme XMLTV fixture.
3. Measure parse time and peak memory.
4. Measure staging and activation time.
5. Require approval for regressions above ten percent.

Completion evidence:

- M3U parsing completes within 15 seconds and 512 MiB RSS.
- M3U activation completes within 90 seconds.
- XMLTV parsing completes within 10 seconds and 512 MiB RSS.
- XMLTV activation completes within 45 seconds.

## Priority 3: Complete Jellyfin Workflows

### P3.1 Correct Jellyfin Setup URLs

State: Pending

Owner: Unassigned

The UI displays hard-coded routes that the backend does not expose.

Work:

1. Load output profile data from the backend.
2. Display tokenized M3U and XMLTV output routes.
3. Display the tokenized HDHomeRun device route.
4. Remove the in-memory Jellyfin settings path.
5. Add copy, rotation, redaction, and expiration tests.

Completion evidence:

- Every displayed URL returns the expected output.
- The UI does not display a raw stored token after navigation.
- Token rotation supports the configured overlap window.

### P3.2 Verify Jellyfin Imports

State: Pending

Owner: Unassigned

Work:

1. Add a Jellyfin service to an acceptance profile.
2. Import the M3U and XMLTV output routes.
3. Import the HDHomeRun device route.
4. Verify the channel subset and stable identifiers.
5. Verify tuner capacity and shared upstream sessions.
6. Add the optional SSDP profile after token warnings exist.

Completion evidence:

- Jellyfin imports channels through M3U and HDHomeRun.
- Jellyfin displays current programme data.
- Multiple Jellyfin clients share each channel upstream.

## Priority 4: Complete Security and Operations

### P4.1 Add OIDC

State: Not implemented

Owner: Unassigned

1. Add the authorization-code flow with PKCE.
2. Validate the configured issuer and client.
3. Apply the approved subject or email allowlist.
4. Retain local administrator break-glass access.
5. Add state, nonce, replay, callback, and logout tests.

### P4.2 Add Operator API Tokens

State: Not implemented

Owner: Unassigned

1. Store only operator token hashes.
2. Add creation, rotation, revocation, and audit events.
3. Show each plaintext token only once.
4. Add scope, expiration, and replay tests.

### P4.3 Complete Revisions and Effective Configuration

State: Partial

Owner: Unassigned

1. Publish backend descriptions and defaults.
2. Return each effective value and inheritance source.
3. State whether each change needs a restart or reimport.
4. Require ETag checks for conflicting edits.
5. Add revision history and rollback tests.

### P4.4 Add Stream Health Jobs

State: Partial

Owner: Unassigned

1. Acquire a low-priority provider slot before each probe.
2. Skip each probe when provider capacity is full.
3. Validate PAT, PMT, audio, video, and sustained packet flow.
4. Store redacted typed failures and quality data.
5. Use current health data for alternate stream rank.

## Priority 5: Complete Product and Documentation Work

### P5.1 Complete the Management UI

State: Partial

Owner: Unassigned

1. Complete source progress, cancellation, and retry workflows.
2. Complete mapping review and rollback workflows.
3. Complete event rule previews.
4. Complete provider pool and adapter policy forms.
5. Complete viewer and upstream diagnostics.
6. Complete support bundle and redacted log workflows.
7. Add accessibility tests for each workflow.

### P5.2 Align Technical Documentation

State: Partial

Owner: Unassigned

1. Remove each claim that lacks current test evidence.
2. Correct the changed-line coverage threshold to 95 percent.
3. Apply ASD-STE100 Issue 9 to each changed technical page.
4. Add source, Jellyfin, security, backup, and recovery procedures.
5. Add OpenAPI client generation and drift checks.
6. Run the strict MkDocs build.

## Scope Control

Do not extend the v1 DVR, multi-user, or stream-profile work.

Keep existing out-of-scope code isolated until the v1 gates pass.

Defer these items:

- DVR, timeshift, catch-up, and VOD.
- Video or audio transcoding.
- DRM support.
- Third-party plugin execution.
- Active-active media clusters.
- GitHub Actions image publication.
- Multi-architecture image publication.
- Software bills of materials.
- Build provenance and image signatures.

## Required Final Verification

1. Run the three-channel test for 120 seconds.
2. Run the six-viewer test for 120 seconds.
3. Run the media soak for 30 minutes.
4. Run fault tests for latency, stalls, resets, and child exits.
5. Run the live provider gateway test.
6. Run the complete Rust and TypeScript coverage gates.
7. Run the dependency, license, security, and documentation gates.
8. Run a clean Docker Compose installation.
9. Verify M3U, XMLTV, and HDHomeRun imports in Jellyfin.

Do not publish v1 until every required gate passes.
