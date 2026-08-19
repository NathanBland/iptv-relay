#!/usr/bin/env sh
set -eu

require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

for command_name in docker docker-compose rustc cargo node pnpm ffmpeg vlc; do
  require "$command_name"
done

docker info >/dev/null
cargo llvm-cov --version >/dev/null
cargo deny --version >/dev/null
cargo +nightly fuzz --version >/dev/null

printf 'docker: %s\n' "$(docker --version)"
printf 'compose: %s\n' "$(docker-compose version --short)"
printf 'rust: %s\n' "$(rustc --version)"
printf 'llvm-cov: %s\n' "$(cargo llvm-cov --version)"
printf 'cargo-deny: %s\n' "$(cargo deny --version)"
printf 'cargo-fuzz: %s\n' "$(cargo +nightly fuzz --version)"
printf 'node: %s\n' "$(node --version)"
printf 'pnpm: %s\n' "$(pnpm --version)"
printf 'ffmpeg: %s\n' "$(ffmpeg -version 2>/dev/null | sed -n '1p')"
printf 'vlc: %s\n' "$(vlc --version 2>/dev/null | sed -n '1p')"
