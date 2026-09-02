#!/usr/bin/env bash
# Test the OpenAPI drift-check script with stubbed curl and jq.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIFT_SCRIPT="$SCRIPT_DIR/openapi-drift-check.sh"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/iptv-openapi-test.XXXXXX")"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin" "$TEST_DIR/scripts" "$TEST_DIR/tests/fixtures"

cat > "$TEST_DIR/bin/curl" <<'CURL_EOF'
#!/usr/bin/env bash
set -euo pipefail
cat "$FAKE_OPENAPI_FILE"
CURL_EOF
chmod +x "$TEST_DIR/bin/curl"

if command -v jq >/dev/null 2>&1; then
  ln -s "$(command -v jq)" "$TEST_DIR/bin/jq"
else
  cat > "$TEST_DIR/bin/jq" <<'JQ_EOF'
#!/usr/bin/env bash
case "$1" in -S) shift ;; esac
python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin), sort_keys=True, indent=2))'
JQ_EOF
  chmod +x "$TEST_DIR/bin/jq"
fi

cp "$DRIFT_SCRIPT" "$TEST_DIR/scripts/openapi-drift-check.sh"
chmod +x "$TEST_DIR/scripts/openapi-drift-check.sh"

DOC_A='{"openapi":"3.0","paths":{"/api/v1/sources":{"get":{}}},"b":2,"a":1}'
DOC_B='{"openapi":"3.0","paths":{"/api/v1/sources":{"get":{}}},"b":3,"a":1}'

printf '%s' "$DOC_A" > "$TEST_DIR/doc-a.json"
printf '%s' "$DOC_B" > "$TEST_DIR/doc-b.json"

# Case 1: live endpoint unreachable fails.
if (cd "$TEST_DIR" && FAKE_OPENAPI_FILE=/tmp/does-not-exist \
  PATH="$TEST_DIR/bin:$PATH" \
  ./scripts/openapi-drift-check.sh http://example.invalid >/dev/null 2>&1); then
  echo "Expected unreachable endpoint to fail." >&2
  exit 1
fi

# Case 2: missing reference fails.
if (cd "$TEST_DIR" && FAKE_OPENAPI_FILE="$TEST_DIR/doc-a.json" \
  PATH="$TEST_DIR/bin:$PATH" \
  ./scripts/openapi-drift-check.sh http://example.invalid >/dev/null 2>&1); then
  echo "Expected missing reference to fail." >&2
  exit 1
fi

# Case 3: --update writes the reference.
(cd "$TEST_DIR" && FAKE_OPENAPI_FILE="$TEST_DIR/doc-a.json" \
  PATH="$TEST_DIR/bin:$PATH" \
  ./scripts/openapi-drift-check.sh --update http://example.invalid >/dev/null)
if [ ! -f "$TEST_DIR/tests/fixtures/openapi.json" ]; then
  echo "Expected --update to create the reference." >&2
  exit 1
fi

# Case 4: matching live doc passes.
if ! (cd "$TEST_DIR" && FAKE_OPENAPI_FILE="$TEST_DIR/doc-a.json" \
  PATH="$TEST_DIR/bin:$PATH" \
  ./scripts/openapi-drift-check.sh http://example.invalid >/dev/null); then
  echo "Expected matching documents to pass." >&2
  exit 1
fi

# Case 5: drifted live doc fails.
if (cd "$TEST_DIR" && FAKE_OPENAPI_FILE="$TEST_DIR/doc-b.json" \
  PATH="$TEST_DIR/bin:$PATH" \
  ./scripts/openapi-drift-check.sh http://example.invalid >/dev/null 2>&1); then
  echo "Expected drifted documents to fail." >&2
  exit 1
fi

echo "OpenAPI drift-check script tests passed."
