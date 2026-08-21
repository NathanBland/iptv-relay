#!/usr/bin/env bash
# Check that changed production lines have test coverage.
# Usage: scripts/changed-line-coverage.sh [threshold] [base-ref]
# threshold: minimum percentage of changed lines that must be covered (default: 95)
# base-ref: git ref to compare against (default: CI base ref, HEAD~1, or the empty tree)

set -euo pipefail

THRESHOLD="${1:-95}"

if ! [[ "$THRESHOLD" =~ ^([0-9]+([.][0-9]+)?)$ ]] || (( $(python3 -c "print(1 if float('$THRESHOLD') > 100 else 0)") )); then
  echo "Threshold must be a number from 0 to 100." >&2
  exit 2
fi

resolve_ref() {
  local requested="$1"
  local candidate
  local candidates=("$requested")

  if [[ "$requested" != refs/* ]]; then
    candidates+=("refs/remotes/origin/$requested" "origin/$requested" "refs/heads/$requested")
  fi

  for candidate in "${candidates[@]}"; do
    if git rev-parse --verify --quiet "${candidate}^{commit}" >/dev/null; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  return 1
}

resolve_base_ref() {
  local requested="${1:-}"
  local environment_ref="${COVERAGE_BASE_REF:-}"
  local candidate

  if [[ -z "$environment_ref" ]]; then
    environment_ref="${GITHUB_BASE_REF:-}"
  fi
  if [[ -z "$environment_ref" ]]; then
    environment_ref="${CI_MERGE_REQUEST_TARGET_BRANCH_SHA:-}"
  fi
  if [[ -z "$environment_ref" ]]; then
    environment_ref="${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:-}"
  fi
  if [[ -z "$environment_ref" ]]; then
    environment_ref="${CI_BASE_REF:-}"
  fi

  if [[ -n "$requested" ]]; then
    if candidate="$(resolve_ref "$requested")"; then
      printf '%s\n' "$candidate"
      return 0
    fi
    echo "Base ref '$requested' does not resolve to a commit." >&2
    return 1
  fi

  if [[ -n "$environment_ref" ]]; then
    if candidate="$(resolve_ref "$environment_ref")"; then
      printf '%s\n' "$candidate"
      return 0
    fi
    echo "CI base ref '$environment_ref' does not resolve to a commit." >&2
    return 1
  fi

  if candidate="$(resolve_ref HEAD~1)"; then
    printf '%s\n' "$candidate"
    return 0
  fi

  # A new repository has no parent commit. Compare it with Git's empty tree.
  git hash-object -t tree /dev/null
}

BASE_REF="$(resolve_base_ref "${2:-}")"

is_production_file() {
  local file="$1"

  case "$file" in
    *.rs|*.ts|*.tsx) ;;
    *) return 1 ;;
  esac

  case "$file" in
    tests/*|*/tests/*|benches/*|*/benches/*|examples/*|*/examples/*|build.rs|*/build.rs|*/bin/*|*.test.*|*.spec.*|e2e/*|*/e2e/*|fuzz/*|*/fuzz/*|migrations/*|*/migrations/*|scripts/*|*/scripts/*|deploy/*|*/deploy/*)
      return 1
      ;;
  esac

  return 0
}

changed_production_files() {
  local file

  while IFS= read -r file; do
    if is_production_file "$file"; then
      printf '%s\n' "$file"
    fi
  done < <(git diff --name-only --diff-filter=ACMR "$BASE_REF" -- '*.rs' '*.ts' '*.tsx')

  # Include new files that are not in the index. Git diff does not list them.
  while IFS= read -r file; do
    if is_production_file "$file"; then
      printf '%s\n' "$file"
    fi
  done < <(git ls-files --others --exclude-standard -- '*.rs' '*.ts' '*.tsx')
}

# Get changed production files.
CHANGED_FILES="$(changed_production_files | sort -u)"

if [ -z "$CHANGED_FILES" ]; then
  echo "No changed production files found."
  exit 0
fi

echo "Changed production files:"
echo "$CHANGED_FILES"
echo ""

# Run the coverage tool for each changed language.
echo "Run coverage analysis..."
COV_JSON=""
WEB_LCOV=""
if printf '%s\n' "$CHANGED_FILES" | grep -qE '\.rs$'; then
  COV_JSON=$(cargo llvm-cov --workspace --all-features --all-targets --ignore-filename-regex 'apps/server/src/bin/(test-provider|media-acceptance|fault-acceptance|live-acceptance)\.rs' --json 2>/dev/null)
fi
if printf '%s\n' "$CHANGED_FILES" | grep -qE '\.(ts|tsx)$'; then
  WEB_LCOV=$(cd apps/web && CI=true pnpm run coverage >&2 && cat coverage/lcov.info)
fi

# Parse coverage and check changed lines.
TOTAL_CHANGED=0
COVERED_CHANGED=0
UNCOVERED_CHANGED=0

while IFS= read -r file; do
  [ -z "$file" ] && continue
  # Get added line numbers from git diff for this file.
  if git ls-files --error-unmatch -- "$file" >/dev/null 2>&1; then
    ADDED_LINE_NUMS=$(git diff --unified=0 "$BASE_REF" -- "$file" | awk '/^@@/ { h=$3; sub(/^\+/, "", h); split(h, a, ","); line=a[1] + 0; next } /^\+\+\+/ { next } /^\+/ { print line; line++; next } /^-/ { next } { line++ }' || true)
  else
    line_count=$(awk 'END { print NR + 0 }' "$file")
    if (( line_count > 0 )); then
      ADDED_LINE_NUMS=$(seq 1 "$line_count")
    else
      ADDED_LINE_NUMS=""
    fi
  fi

  if [ -z "$ADDED_LINE_NUMS" ]; then
    continue
  fi

  # Get uncovered lines from the report for this file.
  case "$file" in
    *.rs)
      COVERAGE_INPUT="$COV_JSON"
      COVERAGE_FORMAT="llvm"
      ;;
    *.ts|*.tsx)
      COVERAGE_INPUT="$WEB_LCOV"
      COVERAGE_FORMAT="lcov"
      ;;
  esac
  COVERAGE_LINES=$(python3 -c '
import json
import sys

target = sys.argv[1]
format_name = sys.argv[2]

if format_name == "lcov":
    current = None
    found = False
    uncovered = set()

    def is_lcov_target(filename):
        if filename == target or filename.endswith("/" + target):
            return True
        web_prefix = "apps/web/"
        if target.startswith(web_prefix):
            web_target = target[len(web_prefix):]
            return filename == web_target or filename.endswith("/" + web_target)
        return False

    for raw_line in sys.stdin:
        line = raw_line.rstrip("\n")
        if line.startswith("SF:"):
            current = line[3:]
            if is_lcov_target(current):
                found = True
        elif line == "end_of_record":
            current = None
        elif current is not None and is_lcov_target(current) and line.startswith("DA:"):
            line_number, count = line[3:].split(",", 1)[:2]
            if int(count.split(",", 1)[0]) == 0:
                uncovered.add(int(line_number))
    if found:
        print("__FILE_FOUND__")
        for line_number in sorted(uncovered):
            print(line_number)
    raise SystemExit(0)

data = json.load(sys.stdin)

def is_target(filename):
    return filename == target or filename.endswith("/" + target)

for file_data in data.get("data", [{}])[0].get("files", []):
    if is_target(file_data.get("filename", "")):
        print("__FILE_FOUND__")

        # Support llvm-cov JSON line, segment, and cargo-llvm-cov line-segment formats.
        lines = file_data.get("lines", [])
        if lines:
            for line_data in lines:
                if line_data.get("count", 0) == 0:
                    print(line_data["line_number"])
            break

        segments = file_data.get("segments", [])
        if segments:
            coverage_by_line = {}
            for segment in segments:
                line = int(segment[0])
                count = int(segment[2])
                coverage_by_line[line] = coverage_by_line.get(line, False) or count > 0
            for line, covered in sorted(coverage_by_line.items()):
                if not covered:
                    print(line)
            break

        for segment in file_data.get("line_segments", []):
            if segment.get("count", 0) == 0:
                start = int(segment["line_start"])
                end = int(segment.get("line_end", start + segment.get("line_count", 1) - 1))
                for line in range(start, end + 1):
                    print(line)
        break
' "$file" "$COVERAGE_FORMAT" <<< "$COVERAGE_INPUT" 2>/dev/null || true)

  if echo "$COVERAGE_LINES" | grep -qx '__FILE_FOUND__'; then
    UNCOVERED_LINES=$(echo "$COVERAGE_LINES" | tail -n +2)
  else
    # Treat a file that is absent from coverage output as uncovered.
    UNCOVERED_LINES="$ADDED_LINE_NUMS"
  fi

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

PERCENTAGE=$(python3 -c "print(f'{($COVERED_CHANGED / $TOTAL_CHANGED * 100):.2f}')")

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
