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
