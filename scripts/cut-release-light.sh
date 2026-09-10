#!/usr/bin/env bash
set -euo pipefail

version="${1:-0.2.17}"
tag="v${version}"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 MAJOR.MINOR.PATCH" >&2
  exit 2
fi

if git rev-parse --verify --quiet "$tag" >/dev/null; then
  echo "tag already exists: $tag" >&2
  exit 1
fi

if [[ "$(git branch --show-current)" != "main" ]]; then
  echo "run this script from the main branch" >&2
  exit 1
fi

sed -i.bak "s/^version = \".*\"/version = \"$version\"/" Cargo.toml
find crates apps -name Cargo.toml -exec \
  sed -i.bak "s/version = \"[0-9]*\.[0-9]*\.[0-9]*\"/version = \"$version\"/g" {} +
find . -name 'Cargo.toml.bak' -delete

# Keep release builds compatible with --locked after the version bump.
cargo check --offline

git add -A
if ! git diff --cached --quiet; then
  git commit -m "Release $tag"
fi

git push origin main
git tag -a "$tag" -m "Release $tag"
git push origin "$tag"
gh release create "$tag" --target main --title "$tag" --generate-notes

echo "Created GitHub release $tag"
