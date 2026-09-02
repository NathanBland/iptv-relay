#!/usr/bin/env bash
# Compare the live /api/v1/openapi.json contract against a checked-in reference.
# Usage: scripts/openapi-drift-check.sh [--update] [base-url]
#
# The OpenAPI endpoint is public and requires no authentication, so the
# drift check runs without secrets.
#
# Set OPENAPI_BASE_URL to override the default base URL.
# Pass --update to write the live document to the reference snapshot.

set -euo pipefail

REFERENCE="tests/fixtures/openapi.json"
DEFAULT_BASE_URL="${OPENAPI_BASE_URL:-http://127.0.0.1:8081}"
UPDATE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --update)
      UPDATE=1
      shift
      ;;
    --help|-h)
      echo "Usage: $0 [--update] [base-url]"
      echo "  --update    Write the live document to ${REFERENCE}"
      echo "  base-url    Core service base URL (default: ${DEFAULT_BASE_URL})"
      exit 0
      ;;
    *)
      DEFAULT_BASE_URL="$1"
      shift
      ;;
  esac
done

BASE_URL="$DEFAULT_BASE_URL"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REFERENCE_PATH="$REPO_ROOT/$REFERENCE"

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required." >&2
  exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required." >&2
  exit 2
fi

echo "Fetch the live OpenAPI document from ${BASE_URL}/api/v1/openapi.json ..."
LIVE_DOC="$(curl --fail --silent --show-error --max-time 15 "${BASE_URL}/api/v1/openapi.json")" || {
  echo "FAIL: The live OpenAPI endpoint did not respond." >&2
  echo "Start the core service, or set OPENAPI_BASE_URL to a running instance." >&2
  exit 1
}

# Normalize both documents so cosmetic key order does not cause drift.
LIVE_NORMALIZED="$(printf '%s' "$LIVE_DOC" | jq -S .)"

if [ "$UPDATE" -eq 1 ]; then
  mkdir -p "$(dirname "$REFERENCE_PATH")"
  printf '%s\n' "$LIVE_NORMALIZED" > "$REFERENCE_PATH"
  echo "Updated the reference snapshot at ${REFERENCE}."
  exit 0
fi

if [ ! -f "$REFERENCE_PATH" ]; then
  echo "FAIL: The reference snapshot is missing at ${REFERENCE}." >&2
  echo "Run: scripts/openapi-drift-check.sh --update" >&2
  exit 1
fi

REFERENCE_NORMALIZED="$(jq -S . "$REFERENCE_PATH")"

if [ "$LIVE_NORMALIZED" = "$REFERENCE_NORMALIZED" ]; then
  echo "PASS: The live OpenAPI contract matches the reference snapshot."
  exit 0
fi

echo "FAIL: The live OpenAPI contract differs from the reference snapshot." >&2
echo "Run the diff below to review the drift:" >&2
echo "  diff <(jq -S . $REFERENCE) <(curl -s ${BASE_URL}/api/v1/openapi.json | jq -S .)" >&2
echo "" >&2
printf '%s\n' "$LIVE_NORMALIZED" > "${TMPDIR:-/tmp}/iptv-openapi-live.json"
diff <(printf '%s\n' "$REFERENCE_NORMALIZED") <(printf '%s\n' "$LIVE_NORMALIZED") >&2 || true
echo "" >&2
echo "When the change is intentional, update the reference:" >&2
echo "  scripts/openapi-drift-check.sh --update" >&2
exit 1
