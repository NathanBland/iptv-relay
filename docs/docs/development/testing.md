# Testing

The project uses Rust tests, TypeScript unit tests, and Playwright end-to-end tests.

## Prerequisites

Start PostgreSQL before integration tests:

```bash
make postgres-test
```

Set the test database URL:

```bash
export IPTV_TEST_DATABASE_URL=postgres://iptv:iptv-development@127.0.0.1:54329/iptv
```

## Rust tests

Run the full Rust workspace test suite:

```bash
make test-rust
```

Run integration tests against PostgreSQL:

```bash
make test-integration
```

## Web tests

Run the TypeScript unit tests:

```bash
make test-web
```

## End-to-end tests

Run the Playwright suite:

```bash
make test-e2e
```

Run end-to-end tests with real sources:

```bash
make test-e2e-real
```

The real-source test skips unless `IPTV_E2E_REAL_SOURCES=true`. This prevents long downloads during routine runs.

## Parallel tests

Run Rust and web tests in parallel:

```bash
make test-parallel
```

The Vitest config uses the `forks` pool with up to four forks. The Playwright config uses `fullyParallel: true` with three workers.

## Coverage

Run the Rust coverage gate:

```bash
make coverage-rust
```

The gate requires 86% line, function, and region coverage.

Run the web coverage gate:

```bash
make coverage-web
```

Run both coverage gates:

```bash
make coverage
```

## Changed-line coverage

Run the changed-line coverage gate:

```bash
make coverage-changed
```

The script compares changed production lines against LLVM coverage data. The threshold defaults to 95 percent.

Override the threshold with the `COVERAGE_CHANGED_THRESHOLD` variable:

```bash
make coverage-changed COVERAGE_CHANGED_THRESHOLD=90
```

Pass a base git ref with `COVERAGE_BASE_REF`:

```bash
make coverage-changed COVERAGE_BASE_REF=origin/main
```

The gate counts only production files. The gate excludes tests, benches, examples, fuzz targets, migrations, scripts, and deploy files.

Run the script self-tests:

```bash
make test-coverage-script
```

## Lint and format

Check Rust formatting:

```bash
make fmt
```

Run Clippy and web type checks:

```bash
make lint
```

Run dependency advisories:

```bash
make audit
```

## Full CI pipeline

Run the full pipeline:

```bash
make ci
```

The `ci` target runs every non-credentialed gate.

The gate runs formatting, lint, audit, Rust and web coverage, and changed-line coverage.

The gate also runs fuzz smoke tests, media acceptance, fault acceptance, Compose health checks, strict documentation build, and Playwright tests.

The gate stops after a required command fails.

Run the live-provider gate separately because it needs provider credentials:

```bash
make live-acceptance
```

The live gate reads an Xtream base URL, username, password, XMLTV URL, and provider capacity from `.env.xtreme`.

The parser accepts only allowlisted `KEY=value` records and does not evaluate shell syntax.
Use `--env-file` when the credential file has another name, such as `.env.live`.

The gate queries Xtream `player_api.php` for live streams and uses the provider `epg_channel_id` values.
It compares those IDs with XMLTV `channel id` values.
Channel names report unmatched data and do not define overlap.

The gate creates a disposable local Compose project with generated credentials and host ports.

The gate verifies all shared provider IDs, programme samples, gateway output, terminal jobs, and three stable sync cycles.
It fails when the provider and XMLTV data have no shared IDs.

The gate reports redacted storage metrics and removes its containers, volumes, and temporary environment file after every result.
The report includes final pending jobs, reconciliation candidates, staging snapshots, temporary files, and cleanup status.

## Compare M3U and Xtream streams

Run the provider stream comparison when playback differs between source types:

```bash
make provider-stream-compare
```

The script reads `IPTV_TEST_M3U_URL` from `.env.live`.
The script reads `URL`, `USER`, and `PWD` from `.env.xtreme`.
The script reads `IPTV_TEST_TARGETS` from `.env.live` or the process environment.
Set that value to a pipe-separated list of exactly two exact provider display names.
The parser does not evaluate either file as shell code.

The script compares the configured records by stream ID.
It uses normalized names only when the M3U record has no usable ID.
It compares URL shapes and private digests without writing provider URLs to the report.

The script probes both direct provider URLs.
It then creates separate disposable M3U and Xtream stacks.
Each stack tests two concurrent streams with the `auto`, `ffmpeg`, and `vlc` input adapters.
The test sets each source `maxConnections` value to the provider capacity.

The source default is one connection.
Set the source value to at least two when the provider permits concurrent streams:

```bash
curl -X PATCH http://localhost:8080/api/v1/sources/{source_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"maxConnections":3}'
```

The script removes its Compose containers, volumes, internal fixture, and temporary files after success or failure.
Use `--no-build` when the local core image already contains the code under test.
Run `make test-provider-stream-compare` for the credential-free helper checks.

Treat `auto` as the baseline adapter for provider MPEG-TS URLs.
Use FFmpeg or VLC only when their two-stream result is also successful for the provider.
Provider adapter failures can be stream-specific even when the URL digest matches.

Run its parser self-test without Docker:

```bash
make test-live-api-acceptance
```

## Isolated test runner

Run the root test runner when you need one command for all non-credentialed gates:

```bash
./scripts/run-test-suite.sh
```

The runner uses a unique Compose project name for each run.

The runner selects free host ports for PostgreSQL and the gateway.

The runner removes its containers, networks, and volumes after the run.

The runner keeps repository build caches to reduce the next run time.

Pass `--keep` when you need to inspect the test services after a failure:

```bash
./scripts/run-test-suite.sh --keep
```

Pass `--live` only when the environment contains authorized provider credentials:

```bash
./scripts/run-test-suite.sh --live
```

The live stage stays disabled by default.

The default run includes build, unit, integration, coverage, acceptance, Compose, scale, and documentation gates.

The default run includes the Jellyfin acceptance gate with the deterministic test provider.

Override `IPTV_TEST_DATABASE_URL`, `IPTV_POSTGRES_PORT`, or `IPTV_GATEWAY_PORT` when the default values conflict.

Run the runner self-test without Docker:

```bash
make test-runner
```

## Acceptance tests

Run the deterministic media acceptance test:

```bash
make media-acceptance
```

Run the fault-injection acceptance test:

```bash
make fault-acceptance
```

Run the live-provider acceptance test:

```bash
make live-acceptance
```

## Fuzz tests

Run the fuzz smoke tests:

```bash
make fuzz-smoke
```

The fuzz targets cover M3U, XMLTV, Xtream, and events parsers.

## Cleanup

Remove test and reconciliation sources:

```bash
make cleanup-test-data
```

The repository test runner preserves shared Cargo caches. Run `make clean-debug` only when you need to remove debug and test artifacts manually.
