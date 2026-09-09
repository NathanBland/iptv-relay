#!/usr/bin/env bash
# Cut a stable semantic-version release from the current main branch.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./cut_release.sh [patch|minor|major|VERSION]

Create a stable GitHub release from main.

The default release type is patch. VERSION must use X.Y.Z format.
The script runs the full repository test suite before it creates a commit or tag.
EOF
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
}

increment_version() {
  local current="$1"
  local release_type="$2"
  local major minor patch

  IFS='.' read -r major minor patch <<<"$current"
  case "$release_type" in
    patch) printf '%s.%s.%s\n' "$major" "$minor" "$((patch + 1))" ;;
    minor) printf '%s.%s.0\n' "$major" "$((minor + 1))" ;;
    major) printf '%s.0.0\n' "$((major + 1))" ;;
    *) printf '%s\n' "$release_type" ;;
  esac
}

release_type="${1:-patch}"
if [[ "$release_type" == "-h" || "$release_type" == "--help" ]]; then
  usage
  exit 0
fi
[[ $# -le 1 ]] || {
  usage >&2
  exit 1
}

for command_name in cargo gh git; do
  require_command "$command_name"
done

root_dir="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this script inside a Git repository"
cd "$root_dir"

[[ "$(git branch --show-current)" == "main" ]] || fail "run releases from the main branch"
[[ -z "$(git status --porcelain)" ]] || fail "commit, stash, or remove every worktree change before a release"
git remote get-url origin >/dev/null || fail "the origin remote is unavailable"
gh auth status >/dev/null || fail "authenticate GitHub CLI before a release"

current_version="$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ "$current_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "Cargo.toml does not contain a stable semantic version"

case "$release_type" in
  patch|minor|major) ;;
  *) [[ "$release_type" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "use patch, minor, major, or X.Y.Z" ;;
esac

next_version="$(increment_version "$current_version" "$release_type")"
[[ "$next_version" != "$current_version" ]] || fail "the release version must change"
release_tag="v${next_version}"

git fetch origin main --tags
git merge-base --is-ancestor origin/main HEAD || fail "main is behind origin/main; update it before a release"
git rev-parse -q --verify "refs/tags/${release_tag}" >/dev/null && fail "tag ${release_tag} already exists locally"
git ls-remote --exit-code --tags origin "refs/tags/${release_tag}" >/dev/null 2>&1 \
  && fail "tag ${release_tag} already exists on origin" || true

base_commit="$(git rev-parse HEAD)"
printf 'Release %s from %s.\n' "$release_tag" "$(git log -1 --format=%h)"

./scripts/run-test-suite.sh

git fetch origin main --tags
[[ "$(git rev-parse origin/main)" == "$base_commit" ]] \
  || fail "origin/main changed during validation; rerun the release from the updated branch"

sed -i.bak "s/^version = \"${current_version}\"$/version = \"${next_version}\"/" Cargo.toml
rm Cargo.toml.bak
cargo check --workspace
cargo check --manifest-path fuzz/Cargo.toml
git diff --check

git add Cargo.toml Cargo.lock fuzz/Cargo.lock
git commit -m "Bump version to ${next_version}"
release_commit="$(git rev-parse HEAD)"
git tag -a "$release_tag" -m "Release ${release_tag}" "$release_commit"
git push origin main "$release_tag"

run_id=""
for _ in $(seq 1 12); do
  run_id="$(gh run list --workflow container.yml --commit "$release_commit" --limit 1 --json databaseId --jq '.[0].databaseId // empty')"
  [[ -n "$run_id" ]] && break
  sleep 5
done
[[ -n "$run_id" ]] || fail "the container workflow did not start for ${release_tag}"

gh run watch "$run_id" --exit-status
gh release create "$release_tag" --verify-tag --generate-notes --title "Release ${release_tag}"

printf 'Release %s is available.\n' "$release_tag"
