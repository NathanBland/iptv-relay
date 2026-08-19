#!/usr/bin/env bash
# Check that changed production lines have test coverage.
# Usage: scripts/changed-line-coverage.sh [threshold] [base-ref]
# threshold: minimum percentage of changed lines that must be covered (default: 80)
# base-ref: git ref to compare against (default: HEAD~1)

set -euo pipefail

THRESHOLD="${1:-80}"
BASE_REF="${2:-HEAD~1}"

# Get changed production files (exclude tests, benches, examples, build scripts, migrations, scripts, deploy).
CHANGED_FILES=$(git diff --name-only --diff-filter=AM "$BASE_REF" -- '*.rs' '*.ts' '*.tsx' | grep -vE '(tests/|benches/|examples/|build\.rs|/bin/|\.test\.|\.spec\.|e2e/|migrations/|scripts/|deploy/)' || true)

if [ -z "$CHANGED_FILES" ]; then
  echo "No changed production files found."
  exit 0
fi

echo "Changed production files:"
echo "$CHANGED_FILES"
echo ""

# Get added line numbers for each changed file.
# Format: file:line
CHANGED_LINES=""
while IFS= read -r file; do
  [ -z "$file" ] && continue
  # Get added line numbers from git diff.
  ADDED_LINES=$(git diff "$BASE_REF" -- "$file" | grep -E '^\+' | grep -vE '^\+\+\+' | sed 's/^+//' || true)
  if [ -n "$ADDED_LINES" ]; then
    CHANGED_LINES="$CHANGED_LINES$ADDED_LINES"
  fi
done <<< "$CHANGED_FILES"

if [ -z "$CHANGED_LINES" ]; then
  echo "No added production lines found."
  exit 0
fi

# Run coverage and get JSON output.
echo "Run coverage analysis..."
COV_JSON=$(cargo llvm-cov --workspace --all-features --all-targets --ignore-filename-regex 'apps/server/src/bin/(test-provider|media-acceptance)\.rs' --json 2>/dev/null)

# Parse coverage and check changed lines.
TOTAL_CHANGED=0
COVERED_CHANGED=0
UNCOVERED_CHANGED=0

while IFS= read -r file; do
  [ -z "$file" ] && continue
  # Get added line numbers from git diff for this file.
  ADDED_LINE_NUMS=$(git diff "$BASE_REF" -- "$file" | awk '/^@@/ { start=$3; gsub(/,/," ",start); split(start, a, " "); line=a[1]; next } /^\+/ && !/^\+\+\+/ { print line; line++ } /^\-/ && !/^\-\-\-/ { next } /^[^@+-]/ { line++ }' || true)

  if [ -z "$ADDED_LINE_NUMS" ]; then
    continue
  fi

  # Get uncovered lines for this file from coverage JSON.
  UNCOVERED_LINES=$(echo "$COV_JSON" | python3 -c "
import json, sys
data = json.load(sys.stdin)
target = '$file'
for f in data.get('data', [{}])[0].get('files', []):
    if f['filename'] == target:
        for segment in f.get('line_segments', []):
            if segment['count'] == 0:
                for line in range(segment['line_start'], segment['line_start'] + segment['line_count']):
                    print(line)
        break
" 2>/dev/null || true)

  for line in $ADDED_LINE_NUMS; do
    TOTAL_CHANGED=$((TOTAL_CHANGED + 1))
    if echo "$UNCOVERED_LINES" | grep -qx "$line"; then
      UNCOVERED_CHANGED=$((UNCOVERED_CHANGED + 1))
    else
      COVERED_CHANGED=$((COVERED_CHANGED + 1))
    fi
  done
done <<< "$CHANGED_FILES"

if [ "$TOTAL_CHANGED" -eq 0 ]; then
  echo "No added production lines found."
  exit 0
fi

PERCENTAGE=$(python3 -c "print(f'{$COVERED_CHANGED / $TOTAL_CHANGED * 100:.2f}')")

echo "Changed production line coverage:"
echo "  Total changed lines: $TOTAL_CHANGED"
echo "  Covered: $COVERED_CHANGED"
echo "  Uncovered: $UNCOVERED_CHANGED"
echo "  Coverage: $PERCENTAGE%"
echo "  Threshold: $THRESHOLD%"

if (( $(python3 -c "print(1 if $PERCENTAGE < $THRESHOLD else 0)") )); then
  echo "FAIL: Changed production line coverage is below the threshold."
  exit 1
fi

echo "PASS: Changed production line coverage meets the threshold."
