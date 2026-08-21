#!/usr/bin/env bash
# Remove test and reconciliation sources from the IPTV gateway database.
# Usage: scripts/cleanup-test-data.sh
#
# Removes:
#   - Provider accounts named "Reconcile account *"
#   - Provider accounts named "Xtream coverage test *"
#   - EPG sources created by integration tests
#   - Orphaned snapshots, streams, and programmes from removed sources
#   - Cancelled or failed jobs older than 24 hours
#
# Does not remove sources configured through the UI or .env.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_DIR"

# Detect the database connection method.
if docker-compose ps postgres 2>/dev/null | grep -q "Up"; then
  PSQL="docker-compose exec -T postgres psql -U iptv -d iptv"
else
  echo "PostgreSQL container is not running. Start it with: docker-compose up -d postgres"
  exit 1
fi

echo "Clean up test and reconciliation sources..."

$PSQL <<'SQL'
BEGIN;

-- Remove test provider accounts.
DELETE FROM channels
WHERE provider_account_id IN (
    SELECT id FROM provider_accounts
    WHERE name LIKE 'Reconcile account %'
       OR name LIKE 'Xtream coverage test %'
)
AND managed_by = 'automatic';

DELETE FROM provider_accounts
WHERE name LIKE 'Reconcile account %'
   OR name LIKE 'Xtream coverage test %';

-- Remove orphaned EPG sources (those without a matching provider account name).
DELETE FROM epg_sources
WHERE name NOT LIKE '%epg%'
  OR id NOT IN (
      SELECT es.id FROM epg_sources es
      WHERE es.enabled = true
        AND EXISTS (
          SELECT 1 FROM source_snapshots ss
          WHERE ss.provider_account_id IS NOT NULL
            AND ss.status = 'active'
        )
  );

-- Remove old completed jobs.
DELETE FROM jobs
WHERE status IN ('succeeded', 'failed', 'cancelled')
  AND completed_at < now() - interval '24 hours';

-- Remove orphaned audit events for deleted sources.
DELETE FROM audit_events
WHERE resource_type = 'source'
  AND resource_id NOT IN (
      SELECT id FROM provider_accounts
      UNION
      SELECT id FROM epg_sources
  );

COMMIT;

-- Report remaining state.
SELECT 'Remaining sources' AS label;
SELECT id, name, source_type FROM provider_accounts ORDER BY name;
SELECT id, name FROM epg_sources ORDER BY name;
SELECT 'Remaining counts' AS label;
SELECT count(*) AS channels FROM channels;
SELECT count(*) AS channel_streams FROM channel_streams;
SELECT count(*) AS programmes FROM programmes;
SQL

echo "Cleanup complete."
