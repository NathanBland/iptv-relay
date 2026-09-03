#!/usr/bin/env bash
set -euo pipefail

files=(deploy/Caddyfile deploy/Caddyfile.dev deploy/Caddyfile.local)
for file in "${files[@]}"; do
  grep -F "header Content-Security-Policy" "$file" >/dev/null
  grep -F "script-src 'self' 'unsafe-inline'" "$file" >/dev/null
  grep -F "script-src-attr 'none'" "$file" >/dev/null
  grep -F "object-src 'none'" "$file" >/dev/null
  grep -F "flush_interval -1" "$file" >/dev/null
done

echo "Caddy security policy checks passed"
