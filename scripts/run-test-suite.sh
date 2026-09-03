#!/usr/bin/env bash
# Run the noncredentialed IPTV Gateway test suite in isolated resources.

set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$ROOT_DIR/.env.test"
RUN_ID="$(date +%s)-$$"
COMPOSE_PROJECT_NAME="iptv-test-${RUN_ID}"
KEEP_RESOURCES=0
RUN_LIVE=0

usage() {
  cat <<'EOF'
Usage: scripts/run-test-suite.sh [options]

Run all noncredentialed repository gates in an isolated Compose project.

Options:
  --live    Run the credentialed live-provider gate after noncredentialed gates.
  --keep    Keep runner-owned Compose resources after the run.
  --help    Show this help.

Environment overrides:
  IPTV_TEST_DATABASE_URL  Database URL used by Rust tests.
  IPTV_POSTGRES_PORT      Host port for the runner PostgreSQL service.
  IPTV_GATEWAY_PORT       Host port for the runner gateway service.
  COVERAGE_BASE_REF       Base revision for changed-line coverage.
EOF
}

while (($# > 0)); do
  case "$1" in
    --live) RUN_LIVE=1 ;;
    --keep) KEEP_RESOURCES=1 ;;
    --help|-h) usage; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing required command: %s\n' "$1" >&2
    exit 2
  }
}

for command_name in docker-compose make cargo node pnpm python3; do
  require_command "$command_name"
done

[[ -f "$ENV_FILE" ]] || {
  printf 'missing test environment file: %s\n' "$ENV_FILE" >&2
  exit 2
}

port_available() {
  python3 - "$1" <<'PY'
import socket
import sys

port = int(sys.argv[1])
with socket.socket() as sock:
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind(("127.0.0.1", port))
    except OSError:
        raise SystemExit(1)
PY
}

pick_port() {
  local port
  for port in "$@"; do
    if port_available "$port"; then
      printf '%s\n' "$port"
      return 0
    fi
  done
  printf 'no available test port found\n' >&2
  return 1
}

export COMPOSE_PROJECT_NAME
export IPTV_POSTGRES_PORT="${IPTV_POSTGRES_PORT:-$(pick_port 55432 55433 55434 55435)}"
export IPTV_GATEWAY_PORT="${IPTV_GATEWAY_PORT:-$(pick_port 18080 18081 18082 18083)}"
export IPTV_PUBLIC_BASE_URL="${IPTV_PUBLIC_BASE_URL:-http://127.0.0.1:${IPTV_GATEWAY_PORT}}"
export IPTV_E2E_BASE_URL="${IPTV_E2E_BASE_URL:-$IPTV_PUBLIC_BASE_URL}"
export IPTV_TEST_DATABASE_URL="${IPTV_TEST_DATABASE_URL:-postgres://iptv:iptv-development@127.0.0.1:${IPTV_POSTGRES_PORT}/iptv}"

cleanup() {
  local exit_code=$?
  if ((KEEP_RESOURCES == 0)); then
    docker-compose --project-name "$COMPOSE_PROJECT_NAME" --env-file "$ENV_FILE" \
      down --volumes --remove-orphans >/dev/null 2>&1 || true
    rm -rf "${RUN_TMP_DIR:-}"
  else
    printf 'kept Compose project: %s\n' "$COMPOSE_PROJECT_NAME" >&2
  fi
  exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

RUN_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/iptv-test-suite.XXXXXX")"
export RUN_TMP_DIR

run_stage() {
  local stage="$1"
  shift
  printf '\n==> %s\n' "$stage"
  set +e
  "$@"
  local status=$?
  set -e
  if ((status == 0)); then
    return 0
  fi
  printf 'FAILED: %s (exit %s)\n' "$stage" "$status" >&2
  return "$status"
}

printf 'test project: %s\n' "$COMPOSE_PROJECT_NAME"
printf 'PostgreSQL port: %s\n' "$IPTV_POSTGRES_PORT"
printf 'Gateway port: %s\n' "$IPTV_GATEWAY_PORT"

run_stage doctor make doctor
run_stage format make fmt
run_stage lint make lint
run_stage audit make audit
run_stage build make build
run_stage test-database make postgres-test
run_stage rust-and-web-tests make test
run_stage integration-tests make test-integration
run_stage coverage make coverage
run_stage changed-line-coverage make coverage-changed
run_stage coverage-script make test-coverage-script
run_stage openapi-tests make test-openapi-drift-check
run_stage caddy-security make test-caddy-security
run_stage fuzz-smoke make fuzz-smoke
run_stage media-acceptance make media-acceptance
run_stage fault-acceptance make fault-acceptance
run_stage jellyfin-acceptance make jellyfin-acceptance
run_stage Compose-health make compose-health
run_stage Compose-config make compose-config
run_stage documentation make docs-build
run_stage browser-acceptance make compose-e2e-ci
run_stage scale-database make postgres-test
run_stage scale-smoke make scale-gate-smoke

if ((RUN_LIVE == 1)); then
  run_stage live-acceptance make live-acceptance
else
  printf '\nCredentialed live acceptance: skipped (use --live to enable).\n'
fi

printf '\nAll requested test stages passed.\n'
