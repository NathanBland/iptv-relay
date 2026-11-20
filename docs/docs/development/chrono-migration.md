# Chrono to Jiff migration plan

## Problem

The `chrono` and `chrono-tz` maintainers stopped maintenance.

RustSec reports two advisories:

- `RUSTSEC-2026-0543` for `chrono`.
- `RUSTSEC-2026-0555` for `chrono-tz`.

The advisories report no vulnerability. `cargo deny check` fails on them, so the release gates fail.

`deny.toml` ignores both advisories as a temporary fix. Remove these entries when the migration is complete.

## Target

Replace `chrono` and `chrono-tz` with `jiff` 0.2.

Jiff gives these replacements:

| Chrono item | Jiff item |
|---|---|
| `DateTime<Utc>` | `jiff::Timestamp` |
| `DateTime<Tz>`, `DateTime<FixedOffset>` | `jiff::Zoned` |
| `NaiveDate` | `jiff::civil::Date` |
| `NaiveDateTime` | `jiff::civil::DateTime` |
| `chrono::Duration` | `jiff::SignedDuration` (exact time) or `jiff::Span` (calendar units) |
| `chrono_tz::Tz` | `jiff::tz::TimeZone` |
| `Utc::now()` | `Timestamp::now()` |
| `format("%…")`, `parse_from_str` | `jiff::fmt::strtime` |
| `to_rfc3339`, `parse_from_rfc3339` | `Display` and `FromStr` on `Timestamp` |

Jiff can bundle the IANA time zone database. Enable the `tzdb-bundle-always` feature. The container image then needs no system zoneinfo files.

## Current use

The survey for this plan found these counts in the workspace:

| Item | Count |
|---|---|
| `DateTime<Utc>` | 150 |
| `to_rfc3339` | 64 |
| `Utc::now` | 44 |
| `Duration::hours` and related | 43 |
| `NaiveDate` | 22 |
| `format("%…")` | 16 |
| `NaiveDateTime` | 9 |
| `parse_from_str` | 6 |
| `parse_from_rfc3339` | 4 |
| `FixedOffset` | 3 |

These crates use `chrono` directly:

- `iptv-domain`
- `iptv-sports`
- `iptv-parsers`
- `iptv-ingest`
- `iptv-persistence`
- `iptv-api`
- `apps/server`

These files use `chrono-tz`:

- `crates/parsers/src/events.rs`
- `crates/parsers/src/xmltv.rs`
- `crates/parsers/src/xtream.rs`
- `crates/persistence/src/catalog.rs`
- `crates/persistence/src/event_scan.rs`
- `iptv-live-acceptance` (development dependency)

Two other dependencies enable chrono features:

- `sqlx` with the `chrono` feature.
- `utoipa` with the `chrono` feature.

## Constraints

1. Keep each JSON wire format the same. The web UI and API clients read these values.
2. Keep XMLTV time strings the same. The format is `%Y%m%d%H%M%S %z`.
3. Keep the database schema the same. Do not add a migration for this work.
4. Keep the OpenAPI document the same, or record each difference in the drift check.

## Risks

### Serialization format

Chrono serializes `DateTime<Utc>` as RFC 3339 with a `Z` suffix and a variable number of fraction digits.

Jiff serializes `Timestamp` as RFC 3339 with a `Z` suffix. The fraction digits can differ.

Before the change, add snapshot tests for each API response that contains a time value. Run these tests before and after each phase.

### Database types

`sqlx` 0.8 has no `jiff` feature. Use the `jiff-sqlx` crate. It gives wrapper types that encode and decode `timestamptz`, `timestamp` and `date`.

The schema has 87 `timestamptz` columns and 8 `interval` columns. Rust code does not decode an `interval` column, so the `interval` columns need no change.

Persistence uses runtime queries, not the `query!` macros. A row read with `try_get::<DateTime<Utc>, _>` changes to a `jiff-sqlx` wrapper type.

### Time zone behavior

`event_scan.rs` uses `America::Denver` and `America::New_York` constants. Replace them with `TimeZone::get("America/Denver")` and related calls.

Jiff and chrono-tz can resolve an ambiguous or missing local time differently. This occurs at a daylight saving time change. Add tests for the spring and fall changes in `America/Denver` and `America/New_York`.

### strftime differences

Jiff `strtime` supports most chrono directives. Check each format string:

- `%Y%m%d%H%M%S %z` in the XMLTV writer and the acceptance binary.
- `%b %-d %-I:%M %p %Z` in `event_scan.rs`.
- `%Y-%m-%d %H:%M:%S` in `xtream.rs`.
- The provider-supplied formats in `events.rs`.

`events.rs` parses formats that come from operator rules. Add a test for each format that a rule can supply.

## Phases

Do each phase in a separate pull request. Each phase must pass the gates and `./scripts/run-test-suite.sh`.

### Phase 0: Tests that lock behavior

1. Add JSON snapshot tests for the API responses that contain time values.
2. Add XMLTV output tests with fixed times.
3. Add daylight saving time tests for event parsing.
4. Add parse tests for each operator date format.

Do not change production code in this phase.

### Phase 1: Leaf crates

Migrate `iptv-domain`, `iptv-sports` and `iptv-parsers`.

1. Add `jiff` to the workspace dependencies.
2. Replace the types and calls in these crates.
3. Replace `chrono-tz` with `jiff::tz` in the parsers.
4. At crate boundaries, convert to chrono for a short time if a caller still needs chrono types.

### Phase 2: Ingest and persistence

Migrate `iptv-ingest` and `iptv-persistence`.

1. Add `jiff-sqlx`.
2. Change row reads and query binds to the `jiff-sqlx` wrapper types.
3. Replace the hardcoded time zones in `event_scan.rs`.
4. Run the Postgres tests in `./scripts/run-test-suite.sh`.

### Phase 3: API and server

Migrate `iptv-api` and `apps/server`.

1. Change `utoipa` from the `chrono` feature to the `jiff_0_2` feature.
2. Replace the 64 `to_rfc3339` calls with `Timestamp` formatting.
3. Remove the temporary chrono conversions from Phase 1.
4. Run the OpenAPI drift check.
5. Run the web typecheck and tests.

### Phase 4: Removal

1. Remove the `chrono` feature from `sqlx`.
2. Remove `chrono` and `chrono-tz` from the workspace dependencies.
3. Remove `chrono-tz` from `iptv-live-acceptance`.
4. Make sure that `cargo tree -i chrono` shows no result in the workspace.
5. Remove the two `ignore` entries from `deny.toml`.
6. Run `cargo deny check`.

## Completion criteria

- `cargo tree -i chrono` and `cargo tree -i chrono-tz` find no workspace path.
- `deny.toml` contains no chrono advisory entry.
- The Phase 0 tests pass without changes to their expected values.
- The OpenAPI drift check passes.
- A release build passes the `container.yml` gates.
