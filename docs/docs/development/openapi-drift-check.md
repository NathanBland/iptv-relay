# OpenAPI Drift Check

The core service serves its live OpenAPI contract at `/api/v1/openapi.json`. The endpoint is public and requires no authentication. The drift check compares the live contract against a checked-in reference snapshot.

## Why the check exists

The reference snapshot records the exact contract that the current code produces. When a handler, schema, or route changes, the live document changes. The drift check fails until you review and update the reference.

This keeps the docs, the frontend, and any external client in sync with the real API.

## Reference snapshot

The reference snapshot lives at `tests/fixtures/openapi.json`. The snapshot is generated from the `ApiDoc` type with the `print-openapi` example. The example needs no database and no secrets.

## Regenerate the snapshot

Run the Makefile target:

```bash
make openapi-snapshot
```

The target runs the `print-openapi` example and writes the normalized document to `tests/fixtures/openapi.json`.

Run the script directly when you need a different output path:

```bash
cargo run -p iptv-api --example print-openapi --quiet 2>/dev/null | jq -S . > tests/fixtures/openapi.json
```

## Run the drift check against a live service

1. Start the core service.
2. Run the Makefile target:

```bash
make openapi-drift-check
```

The script fetches `/api/v1/openapi.json` from `OPENAPI_BASE_URL`. The default value is `http://127.0.0.1:8081`.

Override the base URL when the core service runs elsewhere:

```bash
make openapi-drift-check OPENAPI_BASE_URL=http://core:8081
```

The script prints `PASS` when the live contract matches the reference. The script prints `FAIL` and a diff when the contracts differ.

## Update the reference after an intentional change

When the change is intentional, regenerate the snapshot from the `ApiDoc` type:

```bash
make openapi-snapshot
```

Do not update the reference with the `--update` flag against a live service unless the live service runs the same code as the branch under review. The `print-openapi` example is the deterministic source.

## Run the script self-tests

The drift-check script has self-tests that stub `curl` and `jq`:

```bash
make test-openapi-drift-check
```

The self-tests need no running service.

## CI integration

The `ci` Makefile target runs `test-openapi-drift-check`. Add `openapi-drift-check` to a CI stage that starts the core service first.
