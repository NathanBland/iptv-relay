#!/usr/bin/env bash
# Test changed-line coverage base selection and line accounting.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COVERAGE_SCRIPT="$SCRIPT_DIR/changed-line-coverage.sh"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/iptv-coverage-test.XXXXXX")"
trap 'rm -rf "$TEST_DIR"' EXIT

mkdir -p "$TEST_DIR/bin" "$TEST_DIR/src" "$TEST_DIR/apps/web"
cat > "$TEST_DIR/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s' "$FAKE_COVERAGE_JSON"
EOF
chmod +x "$TEST_DIR/bin/cargo"

cat > "$TEST_DIR/bin/pnpm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p coverage
printf '%s\n' "${FAKE_LCOV:-}" > coverage/lcov.info
EOF
chmod +x "$TEST_DIR/bin/pnpm"

git -C "$TEST_DIR" init -q -b main
git -C "$TEST_DIR" config user.email test@example.invalid
git -C "$TEST_DIR" config user.name coverage-test
git -C "$TEST_DIR" config commit.gpgsign false

printf '%s\n' 'fn baseline() {}' > "$TEST_DIR/src/lib.rs"
printf '%s\n' 'export const baseline = true' > "$TEST_DIR/apps/web/ui.ts"
git -C "$TEST_DIR" add src/lib.rs
git -C "$TEST_DIR" add apps/web/ui.ts
git -C "$TEST_DIR" commit -q -m baseline
git -C "$TEST_DIR" branch base

printf '%s\n' 'fn baseline() {}' 'fn changed() {}' > "$TEST_DIR/src/lib.rs"

(cd "$TEST_DIR" && \
  FAKE_COVERAGE_JSON='{"data":[{"files":[{"filename":"src/lib.rs","lines":[{"line_number":1,"count":1},{"line_number":2,"count":1}]}]}]}' \
  FAKE_LCOV=$'TN:\nSF:ui.ts\nDA:1,1\nLF:1\nLH:1\nend_of_record' \
  PATH="$TEST_DIR/bin:$PATH" \
  COVERAGE_BASE_REF= \
  GITHUB_BASE_REF= \
  "$COVERAGE_SCRIPT" 95)

printf '%s\n' 'fn baseline() {}' 'fn changed() {}' 'fn untracked() {}' > "$TEST_DIR/src/lib.rs"
printf '%s\n' 'fn new_one() {}' 'fn new_two() {}' > "$TEST_DIR/src/new.rs"
printf '%s\n' 'export const baseline = true' 'export const changed = true' > "$TEST_DIR/apps/web/ui.ts"

(cd "$TEST_DIR" && \
  FAKE_COVERAGE_JSON='{"data":[{"files":[{"filename":"/build/src/lib.rs","lines":[{"line_number":1,"count":1},{"line_number":2,"count":1},{"line_number":3,"count":1}]},{"filename":"/build/src/new.rs","lines":[{"line_number":1,"count":1},{"line_number":2,"count":1}]}]}]}' \
  FAKE_LCOV=$'TN:\nSF:/build/apps/web/ui.ts\nDA:1,1\nDA:2,1\nLF:2\nLH:2\nend_of_record' \
  PATH="$TEST_DIR/bin:$PATH" \
  GITHUB_BASE_REF=base \
  "$COVERAGE_SCRIPT" 95)

printf '%s\n' 'export const uncovered = true' > "$TEST_DIR/apps/web/uncovered.ts"
if (cd "$TEST_DIR" && \
  FAKE_COVERAGE_JSON='{"data":[{"files":[]}]}' \
  FAKE_LCOV=$'TN:\nSF:/build/apps/web/uncovered.ts\nDA:1,0\nLF:1\nLH:0\nend_of_record' \
  PATH="$TEST_DIR/bin:$PATH" \
  GITHUB_BASE_REF=base \
  "$COVERAGE_SCRIPT" 95); then
  echo "Expected uncovered TypeScript to fail." >&2
  exit 1
fi

printf '%s\n' 'fn missing_coverage() {}' > "$TEST_DIR/src/missing.rs"
if (cd "$TEST_DIR" && \
  FAKE_COVERAGE_JSON='{"data":[{"files":[]}]}' \
  PATH="$TEST_DIR/bin:$PATH" \
  GITHUB_BASE_REF=base \
  "$COVERAGE_SCRIPT" 95); then
  echo "Expected missing coverage to fail." >&2
  exit 1
fi

mkdir -p "$TEST_DIR/fuzz/fuzz_targets"
printf '%s\n' 'fn fuzz_target() {}' > "$TEST_DIR/fuzz/fuzz_targets/target.rs"
FUZZ_OUTPUT=$(cd "$TEST_DIR" && \
  FAKE_COVERAGE_JSON='{"data":[{"files":[]}]}' \
  PATH="$TEST_DIR/bin:$PATH" \
  GITHUB_BASE_REF=base \
  "$COVERAGE_SCRIPT" 95 || true)
if printf '%s\n' "$FUZZ_OUTPUT" | grep -q 'fuzz/fuzz_targets'; then
  echo "Expected fuzz targets to stay outside the production gate." >&2
  exit 1
fi

echo "Changed-line coverage script tests passed."
