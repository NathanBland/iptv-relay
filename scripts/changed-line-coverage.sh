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
    */playwright.config.ts|*/vitest.config.ts|*/vite.config.ts)
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

  # For Rust files, exclude added lines that live inside #[cfg(test)] items.
  # Test-only code must not count as production lines in the changed-line gate.
  if [[ "$file" == *.rs ]] && [ -n "$ADDED_LINE_NUMS" ]; then
    TEST_LINE_NUMS=$(python3 - "$file" <<'PY' 2>/dev/null || true
import re
import sys

path = sys.argv[1]
try:
    with open(path, 'r', errors='replace') as handle:
        src = handle.read()
except OSError:
    raise SystemExit(0)

line_starts = [0]
for match in re.finditer(r'\n', src):
    line_starts.append(match.end())

def line_of(idx):
    lo, hi = 0, len(line_starts) - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if line_starts[mid] <= idx:
            lo = mid
        else:
            hi = mid - 1
    return lo + 1

def cfg_has_test(attr_text):
    cleaned = re.sub(r'"[^"\\]*(?:\\.[^"\\]*)*"', '""', attr_text)
    if 'cfg' not in cleaned:
        return False
    return re.search(r'\btest\b', cleaned) is not None

def skip_string_comment(pos):
    # Returns new position after skipping a string or comment that starts at pos.
    # Caller already verified the opening character.
    return pos

def find_matching_brace(open_idx):
    depth = 0
    i = open_idx
    in_string = False
    in_char = False
    in_line_comment = False
    in_block_comment = False
    while i < len(src):
        c = src[i]
        if in_line_comment:
            if c == '\n':
                in_line_comment = False
            i += 1
            continue
        if in_block_comment:
            if c == '*' and i + 1 < len(src) and src[i + 1] == '/':
                in_block_comment = False
                i += 2
                continue
            i += 1
            continue
        if in_string:
            if c == '\\':
                i += 2
                continue
            if c == '"':
                in_string = False
            i += 1
            continue
        if in_char:
            if c == '\\':
                i += 2
                continue
            if c == "'":
                in_char = False
            i += 1
            continue
        if c == '/' and i + 1 < len(src):
            if src[i + 1] == '/':
                in_line_comment = True
                i += 2
                continue
            if src[i + 1] == '*':
                in_block_comment = True
                i += 2
                continue
        if c == '"':
            in_string = True
            i += 1
            continue
        if c == "'":
            if i + 1 < len(src) and src[i + 1] == '\\':
                in_char = True
                i += 1
                continue
            if i + 2 < len(src) and src[i + 2] == "'":
                i += 3
                continue
            i += 1
            continue
        if c == '{':
            depth += 1
        elif c == '}':
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1

def skip_attribute(pos):
    # pos points at '#'. Return index after the closing ']'.
    j = pos + 1
    if j < len(src) and src[j] == '!':
        j += 1
    while j < len(src) and src[j] in ' \t':
        j += 1
    if j >= len(src) or src[j] != '[':
        return pos + 1
    k = j + 1
    depth = 1
    in_string = False
    while k < len(src) and depth > 0:
        c = src[k]
        if in_string:
            if c == '\\':
                k += 2
                continue
            if c == '"':
                in_string = False
            k += 1
            continue
        if c == '"':
            in_string = True
            k += 1
            continue
        if c == '[':
            depth += 1
        elif c == ']':
            depth -= 1
        k += 1
    return k

def find_item_end(attr_end):
    # From attr_end, skip whitespace, comments, and other attributes.
    # Then scan the item signature for the first '{' or ';'.
    p = attr_end
    while p < len(src):
        c = src[p]
        if c in ' \t\r\n':
            p += 1
            continue
        if c == '/' and p + 1 < len(src):
            if src[p + 1] == '/':
                while p < len(src) and src[p] != '\n':
                    p += 1
                continue
            if src[p + 1] == '*':
                p += 2
                while p + 1 < len(src) and not (src[p] == '*' and src[p + 1] == '/'):
                    p += 1
                p += 2
                continue
        if c == '#':
            p = skip_attribute(p)
            continue
        break
    q = p
    while q < len(src):
        c = src[q]
        if c == '{':
            return ('brace', q)
        if c == ';':
            return ('semi', q)
        if c == '/' and q + 1 < len(src):
            if src[q + 1] == '/':
                while q < len(src) and src[q] != '\n':
                    q += 1
                continue
            if src[q + 1] == '*':
                q += 2
                while q + 1 < len(src) and not (src[q] == '*' and src[q + 1] == '/'):
                    q += 1
                q += 2
                continue
        if c == '"':
            q += 1
            while q < len(src):
                if src[q] == '\\':
                    q += 2
                    continue
                if src[q] == '"':
                    break
                q += 1
            q += 1
            continue
        q += 1
    return (None, -1)

test_lines = set()
i = 0
n = len(src)
in_string = False
in_char = False
in_line_comment = False
in_block_comment = False
while i < n:
    c = src[i]
    if in_line_comment:
        if c == '\n':
            in_line_comment = False
        i += 1
        continue
    if in_block_comment:
        if c == '*' and i + 1 < n and src[i + 1] == '/':
            in_block_comment = False
            i += 2
            continue
        i += 1
        continue
    if in_string:
        if c == '\\':
            i += 2
            continue
        if c == '"':
            in_string = False
        i += 1
        continue
    if in_char:
        if c == '\\':
            i += 2
            continue
        if c == "'":
            in_char = False
        i += 1
        continue
    if c == '/' and i + 1 < n:
        if src[i + 1] == '/':
            in_line_comment = True
            i += 2
            continue
        if src[i + 1] == '*':
            in_block_comment = True
            i += 2
            continue
    if c == '"':
        in_string = True
        i += 1
        continue
    if c == "'":
        if i + 1 < n and src[i + 1] == '\\':
            in_char = True
            i += 1
            continue
        if i + 2 < n and src[i + 2] == "'":
            i += 3
            continue
        i += 1
        continue
    if c == '#':
        j = i + 1
        if j < n and src[j] == '!':
            j += 1
        while j < n and src[j] in ' \t':
            j += 1
        if j < n and src[j] == '[':
            attr_end = skip_attribute(i)
            attr_text = src[i:attr_end]
            if cfg_has_test(attr_text):
                kind, marker = find_item_end(attr_end)
                if kind == 'brace' and marker >= 0:
                    close = find_matching_brace(marker)
                    if close > marker:
                        start_line = line_of(i)
                        end_line = line_of(close)
                        for ln in range(start_line, end_line + 1):
                            test_lines.add(ln)
                        i = close + 1
                        continue
                elif kind == 'semi' and marker >= 0:
                    start_line = line_of(i)
                    end_line = line_of(marker)
                    for ln in range(start_line, end_line + 1):
                        test_lines.add(ln)
                    i = marker + 1
                    continue
            i = attr_end
            continue
    i += 1

for ln in sorted(test_lines):
    print(ln)
PY
)
    if [ -n "$TEST_LINE_NUMS" ]; then
      ADDED_LINE_NUMS=$(printf '%s\n' "$ADDED_LINE_NUMS" | grep -vxFf <(printf '%s\n' "$TEST_LINE_NUMS") || true)
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
            executable_by_line = {}
            for segment in segments:
                line = int(segment[0])
                count = int(segment[2])
                coverage_by_line[line] = coverage_by_line.get(line, False) or count > 0
                executable_by_line[line] = executable_by_line.get(line, False) or bool(segment[3])
            # LLVM can emit zero-count continuation segments for a statement
            # that executes on an adjacent line. Do not report punctuation-only
            # continuation lines as misses when the surrounding statement ran.
            source_lines = {}
            try:
                with open(target, encoding="utf-8") as source:
                    source_lines = {index: value.strip() for index, value in enumerate(source, 1)}
            except OSError:
                pass
            for line, executable in list(executable_by_line.items()):
                if executable or coverage_by_line.get(line, False):
                    continue
                text = source_lines.get(line, "")
                if not text.startswith((".", ")", "?", ",")):
                    continue
                if coverage_by_line.get(line - 1, False) or coverage_by_line.get(line + 1, False):
                    coverage_by_line[line] = True
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
