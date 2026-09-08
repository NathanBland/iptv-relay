//! Bounded cleanup for ingest staging and reconciliation state.
use crate::PersistenceError;
use sqlx::PgPool;
use uuid::Uuid;

/// Counts snapshot and run headers removed by one cleanup pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IngestCleanupStats {
    pub reconciliation_runs: u64,
    pub snapshots: u64,
    pub rows_removed: u64,
}

#[derive(Clone, Debug)]
pub struct IngestCleanupRepository {
    pool: PgPool,
}

impl IngestCleanupRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn cleanup(
        &self,
        retention_seconds: i64,
    ) -> Result<IngestCleanupStats, PersistenceError> {
        self.cleanup_batch(retention_seconds, 100).await
    }

    /// Removes bounded child rows before their obsolete snapshot and run headers.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn cleanup_batch(
        &self,
        retention_seconds: i64,
        batch_size: i64,
    ) -> Result<IngestCleanupStats, PersistenceError> {
        let retention_seconds = retention_seconds.max(60);
        let batch_size = batch_size.clamp(1, 5_000);
        let mut tx = self.pool.begin().await?;
        let mut rows_removed = 0;
        sqlx::query("SET LOCAL statement_timeout = '10s'")
            .execute(&mut *tx)
            .await?;
        // A terminal parent's intents cannot dispatch and must not pin data forever.
        rows_removed += sqlx::query("DELETE FROM job_outbox WHERE id IN (SELECT o.id FROM job_outbox o JOIN jobs j ON j.id = o.parent_job_id WHERE j.status IN ('succeeded', 'failed', 'cancelled') AND j.locked_by IS NULL ORDER BY o.created_at LIMIT $1 FOR UPDATE OF o SKIP LOCKED)")
            .bind(batch_size).execute(&mut *tx).await?.rows_affected();
        let run_ids: Vec<Uuid> = sqlx::query_scalar(r"
            SELECT r.id FROM provider_reconciliation_runs r
            WHERE r.updated_at < now() - make_interval(secs => $1)
              AND NOT EXISTS (SELECT 1 FROM jobs j WHERE (j.status IN ('queued', 'running') OR j.locked_by IS NOT NULL)
                AND (j.id = r.parent_job_id OR j.payload->>'runId' = r.id::text))
              AND NOT EXISTS (SELECT 1 FROM job_outbox o WHERE o.dispatched_at IS NULL AND (o.payload->>'runId' = r.id::text OR o.payload->>'snapshotId' = r.source_snapshot_id::text))
            ORDER BY r.updated_at LIMIT $2 FOR UPDATE OF r SKIP LOCKED
        ").bind(retention_seconds).bind(batch_size).fetch_all(&mut *tx).await?;
        rows_removed += sqlx::query("DELETE FROM provider_reconciliation_candidate_streams WHERE ctid IN (SELECT ctid FROM provider_reconciliation_candidate_streams WHERE run_id = ANY($1) LIMIT $2)")
            .bind(&run_ids).bind(batch_size).execute(&mut *tx).await?.rows_affected();
        rows_removed += sqlx::query("DELETE FROM provider_reconciliation_candidates c WHERE c.ctid IN (SELECT c2.ctid FROM provider_reconciliation_candidates c2 WHERE run_id = ANY($1) AND NOT EXISTS (SELECT 1 FROM provider_reconciliation_candidate_streams s WHERE s.run_id = c2.run_id AND s.canonical_key = c2.canonical_key) LIMIT $2)")
            .bind(&run_ids).bind(batch_size).execute(&mut *tx).await?.rows_affected();
        let runs = sqlx::query("DELETE FROM provider_reconciliation_runs r WHERE id = ANY($1) AND NOT EXISTS (SELECT 1 FROM provider_reconciliation_candidates c WHERE c.run_id = r.id)")
            .bind(&run_ids).execute(&mut *tx).await?.rows_affected();
        let snapshot_ids: Vec<Uuid> = sqlx::query_scalar(r"
            SELECT s.id FROM source_snapshots s
            WHERE s.status <> 'active' AND s.fetched_at < now() - make_interval(secs => $1)
              AND NOT EXISTS (SELECT 1 FROM jobs j WHERE (j.status IN ('queued', 'running') OR j.locked_by IS NOT NULL)
                AND (j.payload->>'sourceId' IN (s.provider_account_id::text, s.epg_source_id::text)
                     OR j.payload->>'snapshotId' = s.id::text OR j.payload->>'sourceSnapshotId' = s.id::text))
              AND NOT EXISTS (SELECT 1 FROM job_outbox o WHERE o.dispatched_at IS NULL AND o.payload->>'snapshotId' = s.id::text)
              AND NOT EXISTS (SELECT 1 FROM provider_reconciliation_runs r WHERE r.source_snapshot_id = s.id)
              AND NOT EXISTS (SELECT 1 FROM provider_streams ps JOIN channel_streams cs ON cs.provider_stream_id = ps.id WHERE ps.snapshot_id = s.id)
              AND NOT EXISTS (SELECT 1 FROM epg_channels ec JOIN channel_epg_mappings cem ON cem.epg_channel_id = ec.id WHERE ec.source_snapshot_id = s.id)
            ORDER BY s.fetched_at LIMIT $2 FOR UPDATE OF s SKIP LOCKED
        ").bind(retention_seconds).bind(batch_size).fetch_all(&mut *tx).await?;
        rows_removed += sqlx::query("DELETE FROM programmes WHERE id IN (SELECT id FROM programmes WHERE source_snapshot_id = ANY($1) LIMIT $2)")
            .bind(&snapshot_ids).bind(batch_size).execute(&mut *tx).await?.rows_affected();
        rows_removed += sqlx::query("DELETE FROM epg_channels e WHERE id IN (SELECT id FROM epg_channels e2 WHERE source_snapshot_id = ANY($1) AND NOT EXISTS (SELECT 1 FROM programmes p WHERE p.epg_channel_id = e2.id) LIMIT $2)")
            .bind(&snapshot_ids).bind(batch_size).execute(&mut *tx).await?.rows_affected();
        rows_removed += sqlx::query("DELETE FROM provider_streams WHERE id IN (SELECT id FROM provider_streams WHERE snapshot_id = ANY($1) LIMIT $2)")
            .bind(&snapshot_ids).bind(batch_size).execute(&mut *tx).await?.rows_affected();
        let snapshots = sqlx::query("DELETE FROM source_snapshots s WHERE id = ANY($1) AND NOT EXISTS (SELECT 1 FROM provider_streams p WHERE p.snapshot_id = s.id) AND NOT EXISTS (SELECT 1 FROM epg_channels e WHERE e.source_snapshot_id = s.id) AND NOT EXISTS (SELECT 1 FROM programmes p WHERE p.source_snapshot_id = s.id)")
            .bind(&snapshot_ids).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(IngestCleanupStats {
            reconciliation_runs: runs,
            snapshots,
            rows_removed: rows_removed + runs + snapshots,
        })
    }
}
