#!/usr/bin/env bash
set -euo pipefail

files=(deploy/Caddyfile deploy/Caddyfile.dev deploy/Caddyfile.local)
for file in "${files[@]}"; do
  grep -F "header Content-Security-Policy" "$file" >/dev/null
  grep -F "script-src 'self'" "$file" >/dev/null
  grep -F "object-src 'none'" "$file" >/dev/null
done

echo "Caddy security policy checks passed"
