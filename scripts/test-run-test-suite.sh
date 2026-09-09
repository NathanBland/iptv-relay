#!/usr/bin/env bash
# Test the isolated test-suite runner without starting Docker services.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="$SCRIPT_DIR/run-test-suite.sh"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/iptv-test-runner.XXXXXX")"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin"
cat > "$TEST_DIR/bin/docker-compose" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker-compose %s\n' "$*" >> "$FAKE_LOG"
EOF
chmod +x "$TEST_DIR/bin/docker-compose"

cat > "$TEST_DIR/bin/make" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
stage="${1:-}"
printf 'make %s\n' "$stage" >> "$FAKE_LOG"
if [[ "${FAKE_MAKE_FAIL_STAGE:-}" == "$stage" ]]; then
  exit 17
fi
EOF
chmod +x "$TEST_DIR/bin/make"

for command_name in cargo node pnpm; do
  cat > "$TEST_DIR/bin/$command_name" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
  chmod +x "$TEST_DIR/bin/$command_name"
done

run_runner() {
  PATH="$TEST_DIR/bin:$PATH" \
    FAKE_LOG="$TEST_DIR/log" \
    IPTV_POSTGRES_PORT=55435 \
    IPTV_GATEWAY_PORT=18083 \
    "$RUNNER" "$@"
}

: > "$TEST_DIR/log"
SECRET_VALUE='runner-secret-must-not-print'
RUN_OUTPUT=$(IPTV_ADMIN_BOOTSTRAP_TOKEN="$SECRET_VALUE" IPTV_PROVIDER_URL="https://user:password@example.invalid/live" run_runner)
if grep -Fq "$SECRET_VALUE" <<<"$RUN_OUTPUT"; then
  echo 'The runner must not print secret environment values.' >&2
  exit 1
fi
grep -q '^make doctor$' "$TEST_DIR/log"
grep -q '^make postgres-test$' "$TEST_DIR/log"
grep -q '^make scale-gate-smoke$' "$TEST_DIR/log"
if grep -q '^make live-acceptance$' "$TEST_DIR/log"; then
  echo 'Live acceptance must stay opt-in.' >&2
  exit 1
fi
grep -q -- 'down --volumes --remove-orphans' "$TEST_DIR/log"

: > "$TEST_DIR/log"
run_runner --live >/dev/null
grep -q '^make live-acceptance$' "$TEST_DIR/log"

: > "$TEST_DIR/log"
run_runner --live-jellyfin >/dev/null
grep -q '^make live-jellyfin-acceptance$' "$TEST_DIR/log"

: > "$TEST_DIR/log"
set +e
FAIL_OUTPUT=$(FAKE_MAKE_FAIL_STAGE=lint run_runner 2>&1 >/dev/null)
FAIL_STATUS=$?
set -e
if ((FAIL_STATUS != 17)) || ! grep -q 'FAILED: lint (exit 17)' <<<"$FAIL_OUTPUT"; then
  echo 'Expected a failed stage to fail the runner.' >&2
  exit 1
fi
grep -q '^make lint$' "$TEST_DIR/log"
if grep -q '^make audit$' "$TEST_DIR/log"; then
  echo 'Runner must stop after the failed stage.' >&2
  exit 1
fi
grep -q -- 'down --volumes --remove-orphans' "$TEST_DIR/log"

: > "$TEST_DIR/log"
run_runner --keep >/dev/null 2>&1
if grep -q -- 'down --volumes --remove-orphans' "$TEST_DIR/log"; then
  echo 'The --keep option must preserve runner resources.' >&2
  exit 1
fi

echo 'Test-suite runner tests passed.'
