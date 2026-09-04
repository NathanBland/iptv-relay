#!/usr/bin/env bash
# Static regression tests for the pull-only release Compose distribution.
#
# These tests reject:
#   - build: keys in the release Compose file
#   - repository bind mounts (relative host paths)
#   - inconsistent application image tags
#   - missing health checks on release services
#   - missing release metadata in the container workflow
#
# The tests use docker-compose config for structural validation and jq for
# field checks. They do not start any container.

set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.appliance.yml"
WORKFLOW_FILE="$ROOT_DIR/.github/workflows/container.yml"
GATEWAY_DOCKERFILE="$ROOT_DIR/Dockerfile.gateway"
WEB_DOCKERFILE="$ROOT_DIR/apps/web/Dockerfile"
CORE_DOCKERFILE="$ROOT_DIR/Dockerfile"

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing required command: %s\n' "$1" >&2
    exit 2
  }
}

# Detect the docker compose command (v2 plugin or v1 standalone).
if docker compose version >/dev/null 2>&1; then
  DOCKER_COMPOSE=(docker compose)
elif command -v docker-compose >/dev/null 2>&1; then
  DOCKER_COMPOSE=(docker-compose)
else
  printf 'missing required command: docker compose or docker-compose\n' >&2
  exit 2
fi

for command_name in jq grep; do
  require_command "$command_name"
done

[ -f "$COMPOSE_FILE" ] || { printf 'missing release compose file: %s\n' "$COMPOSE_FILE" >&2; exit 1; }
[ -f "$WORKFLOW_FILE" ] || { printf 'missing container workflow: %s\n' "$WORKFLOW_FILE" >&2; exit 1; }
[ -f "$GATEWAY_DOCKERFILE" ] || { printf 'missing gateway Dockerfile: %s\n' "$GATEWAY_DOCKERFILE" >&2; exit 1; }

# Resolve the release Compose config with a stable IPTV_VERSION and the
# tracked appliance env example so the file interpolates without operator
# secrets. The env example contains only placeholders, not real secrets.
CONFIG_JSON="$(IPTV_VERSION=latest "${DOCKER_COMPOSE[@]}" -f "$COMPOSE_FILE" --env-file "$ROOT_DIR/.env.appliance.example" config --format json 2>/dev/null)"

fail() {
  printf 'release compose regression failed: %s\n' "$1" >&2
  exit 1
}

# 1. Reject build: keys in the release Compose file.
build_count=$(printf '%s' "$CONFIG_JSON" | jq '[.services[] | select(has("build"))] | length')
[ "$build_count" = "0" ] || fail "release compose must not define build contexts (found ${build_count})"

# 2. Reject repository bind mounts (relative host paths under the source tree).
bind_count=$(printf '%s' "$CONFIG_JSON" | jq '[.services[].volumes[]? | select(.type == "bind") | select(.source | test("^\\."))] | length')
[ "$bind_count" = "0" ] || fail "release compose must not bind-mount repository paths (found ${bind_count})"

# 3. All application images must use the same IPTV_VERSION variable and the
#    ghcr.io/nathanbland/iptv-relay registry prefix.
app_services='["gateway","web","core","worker"]'
image_count=$(printf '%s' "$CONFIG_JSON" | jq --argjson apps "$app_services" '[.services | to_entries[] | select(.key as $k | $apps | index($k))] | length')
[ "$image_count" = "4" ] || fail "expected four application services, found ${image_count}"

bad_images=$(printf '%s' "$CONFIG_JSON" | jq --argjson apps "$app_services" -r '
  [.services | to_entries[] | select(.key as $k | $apps | index($k)) | .value.image]
  | map(select(test("ghcr.io/nathanbland/iptv-relay") | not))
  | length')
[ "$bad_images" = "0" ] || fail "application images must use the ghcr.io/nathanbland/iptv-relay prefix"

# All resolved application images must share the same tag (IPTV_VERSION).
tag_set=$(printf '%s' "$CONFIG_JSON" | jq --argjson apps "$app_services" -r '
  [.services | to_entries[] | select(.key as $k | $apps | index($k)) | .value.image]
  | map(sub(".*:"; ""))
  | unique | length')
[ "$tag_set" = "1" ] || fail "application images must use one consistent IPTV_VERSION tag"

# 4. Health checks must be present on every release service.
required_services='["gateway","web","core","worker","postgres"]'
missing_health=$(printf '%s' "$CONFIG_JSON" | jq --argjson req "$required_services" -r '
  [$req[] as $s | {service: $s, has: (.services[$s] | has("healthcheck"))}]
  | map(select(.has | not) | .service) | join(", ")')
[ -z "$missing_health" ] || fail "missing health checks on: ${missing_health}"

# Restart policies must be present on every release service.
missing_restart=$(printf '%s' "$CONFIG_JSON" | jq --argjson req "$required_services" -r '
  [$req[] as $s | {service: $s, has: (.services[$s] | has("restart"))}]
  | map(select(.has | not) | .service) | join(", ")')
[ -z "$missing_restart" ] || fail "missing restart policies on: ${missing_restart}"

# Named PostgreSQL storage must be present.
pg_volume=$(printf '%s' "$CONFIG_JSON" | jq -r '[.services.postgres.volumes[]? | select(.type == "volume" and .source == "postgres-data")] | length')
[ "$pg_volume" -ge "1" ] || fail "postgres must use the postgres-data named volume"

# The public gateway port must be published.
gateway_port=$(printf '%s' "$CONFIG_JSON" | jq -r '[.services.gateway.ports[]? | select(.target == 8080)] | length')
[ "$gateway_port" -ge "1" ] || fail "gateway must publish the public port 8080"

# 5. Release metadata: the workflow must use docker/metadata-action, buildx,
#    SBOM, provenance, Cosign signing, and Cosign verification for every image.
grep -F 'docker/metadata-action@v5' "$WORKFLOW_FILE" >/dev/null || fail "workflow must use docker/metadata-action@v5"
grep -F 'docker/build-push-action@v6' "$WORKFLOW_FILE" >/dev/null || fail "workflow must use docker/build-push-action@v6"
grep -F 'sbom: true' "$WORKFLOW_FILE" >/dev/null || fail "workflow must enable SBOM generation"
grep -F 'provenance: mode=max' "$WORKFLOW_FILE" >/dev/null || fail "workflow must enable max provenance"
grep -F 'sigstore/cosign-installer@v3' "$WORKFLOW_FILE" >/dev/null || fail "workflow must install Cosign"
grep -F 'cosign sign --yes' "$WORKFLOW_FILE" >/dev/null || fail "workflow must sign image digests"
grep -F 'cosign verify' "$WORKFLOW_FILE" >/dev/null || fail "workflow must verify image signatures"

# The workflow must build both linux/amd64 and linux/arm64 platforms.
grep -F 'linux/amd64' "$WORKFLOW_FILE" >/dev/null || fail "workflow must build linux/amd64"
grep -F 'linux/arm64' "$WORKFLOW_FILE" >/dev/null || fail "workflow must build linux/arm64"

# The workflow must use native arm64 runners (not QEMU emulation).
grep -F 'ubuntu-24.04-arm' "$WORKFLOW_FILE" >/dev/null || fail "workflow must use native arm64 runners"

# The workflow must emit semantic version tags in the manifest job.
grep -F 'VERSION="${REF_TAG#v}"' "$WORKFLOW_FILE" >/dev/null || fail "workflow must extract semver version from tag"
grep -F 'MAJOR_MINOR' "$WORKFLOW_FILE" >/dev/null || fail "workflow must emit major.minor tags"
grep -F 'MAJOR' "$WORKFLOW_FILE" >/dev/null || fail "workflow must emit major version tags"

# 6. Latest-tag gating: the latest tag must be gated so that only stable
#    tag pushes publish latest. Prerelease tags (hyphenated) and manual
#    workflow_dispatch runs must not publish latest.
grep -F 'IS_STABLE' "$WORKFLOW_FILE" >/dev/null || fail "workflow must define IS_STABLE variable"
grep -F "github.event_name == 'push'" "$WORKFLOW_FILE" >/dev/null || fail "latest tag must enable only on push events"
grep -F '!contains(github.ref_name' "$WORKFLOW_FILE" >/dev/null || fail "latest tag must be disabled for hyphenated prerelease tags"
grep -F "'-'" "$WORKFLOW_FILE" >/dev/null || fail "latest tag must test the ref name for a hyphen"

# Least-privilege: the v1-gates job must not request packages or id-token
# permissions.
grep -A4 'name: Run v1 gates' "$WORKFLOW_FILE" | grep -F 'packages: write' && \
  fail "v1-gates job must not request packages: write" || true
grep -A4 'name: Run v1 gates' "$WORKFLOW_FILE" | grep -F 'id-token: write' && \
  fail "v1-gates job must not request id-token: write" || true

# The gateway Dockerfile must bake in the Caddy configuration.
grep -F 'COPY deploy/Caddyfile /etc/caddy/Caddyfile' "$GATEWAY_DOCKERFILE" >/dev/null || \
  fail "gateway Dockerfile must copy deploy/Caddyfile into the image"

# The release Compose file must not mount deploy/Caddyfile from the host.
caddy_mount=$(printf '%s' "$CONFIG_JSON" | jq '[.services.gateway.volumes[]? | select(.source | test("Caddyfile"))] | length')
[ "$caddy_mount" = "0" ] || fail "release gateway must not bind-mount a host Caddyfile"

echo "Release compose regression tests passed."
