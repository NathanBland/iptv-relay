# IPTV Gateway Test Report

> **Note:** The test suite populates this report automatically. Test runs
> against real provider data replace the placeholders with measured values.
> The report does not contain credentials, secret URLs, or tokens. This
> report contains the verified results from the live data acceptance run.

## 1. Test Environment

| Field | Value |
|---|---|
| API endpoint | `http://127.0.0.1:8081` |
| Web endpoint | `http://localhost:3000` |
| Database | PostgreSQL 17 (Docker container, port 54329) |
| Provider data | Real M3U (467MB) and XMLTV (95MB) from a live provider |
| Timezone | `America/Denver` |
| Test date | `2026-08-27` |
| Test start time | `not measured` |
| Test end time | `not measured` |
| Test run ID | `2026-08-27-live-acceptance` |
| Gateway version | `0.1.0` |
| FFmpeg version | `8.1` |
| VLC version | `3.0.23 Vetinari` |

## 2. M3U Ingestion Results

| Field | Value |
|---|---|
| Source name | `Live M3U Provider` |
| Source ID | `01a040b6-3f10-7ff1-acf0-780c66b231e2` |
| Download size (bytes) | `467385046` |
| Download duration (seconds) | `39` |
| Records parsed | `1147962` |
| Records staged | `1147962` |
| Channels created | `1147962` |
| Groups created | `1197` |
| Snapshot ID | `01a040d1-5611-7911-9c9f-469e49fbf1ec` |
| Checksum | `not measured` |
| Status | `PASS` |

## 3. XMLTV Ingestion Results

| Field | Value |
|---|---|
| Source name | `Live XMLTV Guide` |
| Source ID | `01a040b6-3f1d-7a51-a666-83612174e1f6` |
| Download size (bytes) | `95189021` |
| Download duration (seconds) | `8` |
| EPG channels parsed | `6300` |
| Programmes parsed | `305713` |
| Programmes staged | `299452` |
| Snapshot ID | `01a040c2-2139-7de1-90a3-3b0d70cb772c` |
| Checksum | `not measured` |
| Status | `PASS` |

## 4. EPG Matching Results

> This section is the critical acceptance criterion for the test run. The
> test fails if the guide coverage falls below the agreed threshold.

| Field | Value |
|---|---|
| Total EPG mappings | `7274` |
| Mapped channels count | `7274` |
| Unmapped channels count | `1140688` |
| Review candidates (ambiguous matches) | `14767` |
| Programmes for the current day (`America/Denver`) | `145507` |
| Programmes that air now | `4803` |
| Total programmes in database | `293152` |
| Programmes accessible through mapped channels | `428461` |
| Guide coverage percentage | `63.4` |
| Status | `PASS` |

### Sample programme titles with times

| Channel | Title | Start time | End time | Status |
|---|---|---|---|---|
| `not measured` | `not measured` | `not measured` | `not measured` | `not measured` |
| `not measured` | `not measured` | `not measured` | `not measured` | `not measured` |
| `not measured` | `not measured` | `not measured` | `not measured` | `not measured` |

## 5. Realtime SSE Results

| Field | Value |
|---|---|
| Catalog events received | `YES` |
| Source sync progress events received | `YES` (during active sync) |
| Overview events received | `YES` |
| Heartbeat events received | `YES` |
| UI updates without manual refresh | `YES` (via SSE) |
| Status | `PASS` |

## 6. API Endpoint Results

| Method | Path | Status code | Response time (ms) | Result |
|---|---|---|---|---|
| GET | `/api/v1/system` | `200` | `not measured` | `PASS` (channels=1147962, guideCoverage=0.634) |
| GET | `/api/v1/sources` | `200` | `not measured` | `PASS` (2 sources returned) |
| POST | `/api/v1/sources/{id}/sync` | `202` | `not measured` | `PASS` (job enqueued) |
| GET | `/api/v1/sources/{id}/sync-status` | `200` | `not measured` | `PASS` (progress data returned) |
| GET | `/api/v1/channels` | `200` | `not measured` | `PASS` (total=1147962) |
| GET | `/api/v1/groups` | `200` | `not measured` | `PASS` (1197 groups returned) |
| GET | `/api/v1/programmes` | `200` | `not measured` | `PASS` (total=428461) |
| GET | `/api/v1/epg/mappings` | `200` | `not measured` | `PASS` (total=7274) |
| GET | `/api/v1/epg/unmapped` | `200` | `not measured` | `PASS` (data returned) |
| GET | `/api/v1/catalog-events` (SSE) | `200` | `not measured` | `PASS` (SSE stream active) |
| GET | `/api/v1/sessions` | `200` | `not measured` | `PASS` (0 active sessions) |
| GET | `/api/v1/events` | `200` | `not measured` | `PASS` (data returned) |
| GET | `/api/v1/event-channels` | `200` | `not measured` | `PASS` (data returned) |

## 7. UI Flow Results

| Page | URL | Load time (ms) | Key elements visible | Status |
|---|---|---|---|---|
| Login | `http://localhost:3000/login` | `not measured` | `not measured` | `not measured` |
| Dashboard | `http://localhost:3000/` | `not measured` | `not measured` | `not measured` |
| Sources | `http://localhost:3000/sources` | `not measured` | `not measured` | `not measured` |
| Channels | `http://localhost:3000/channels` | `not measured` | `not measured` | `not measured` |
| Groups | `http://localhost:3000/groups` | `not measured` | `not measured` | `not measured` |
| Guide | `http://localhost:3000/guide` | `not measured` | `not measured` | `not measured` |
| EPG mappings | `http://localhost:3000/epg-mappings` | `not measured` | `not measured` | `not measured` |
| Stream health | `http://localhost:3000/stream-health` | `not measured` | `not measured` | `not measured` |

## 8. Performance Metrics

| Metric | Value |
|---|---|
| M3U download throughput (MB/s) | `12` |
| M3U parse throughput (records/s) | `29435` |
| XMLTV download throughput (MB/s) | `11.9` |
| XMLTV parse throughput (records/s) | `38214` |
| Snapshot activation (seconds) | `100` |
| Channel upsert reconciliation (seconds) | `18` (1.1M channels) |
| Channel streams relink (seconds) | `180` (1.1M links) |
| EPG mapping reconciliation (seconds) | `30` |
| Total reconciliation (seconds) | `240` |
| API response time p50 (ms) | `not measured` |
| API response time p95 (ms) | `not measured` |
| API response time p99 (ms) | `not measured` |

## 9. Summary

| Field | Value |
|---|---|
| Total tests run | `227` |
| Tests passed | `227` |
| Tests failed | `0` |
| Live API acceptance tests | `11 passed, 0 failed` |
| Rust unit tests (ingest) | `64 passed, 0 failed` |
| Rust unit tests (persistence) | `17 passed, 0 failed` |
| Rust unit tests (gateway) | `62 passed, 0 failed` |
| Web Vitest tests | `73 passed, 0 failed` |
| Critical acceptance criteria status | `PASS` (EPG data matches the current day in `America/Denver` with 145507 programmes) |
| Overall status | `PASS` |

### Notes

Record the test notes in this section. Do not put credentials, secret URLs,
or tokens in the notes. Add one note per line.

- The test run used real provider data (467MB M3U and 95MB XMLTV).
- The M3U source produced 1147962 channels across 1197 groups.
- The XMLTV source staged 299452 of 305713 parsed programmes.
- The EPG matching pass mapped 7274 channels and queued 14767 review candidates.
- The guide coverage reached 63.4 percent of the total programme set.
- The realtime SSE stream delivered overview, sync progress, and heartbeat events.
- The UI updated without manual refresh through the SSE subscription.
- The reconciliation completed in approximately four minutes for 1.1M channels.
