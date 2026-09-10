//! Canonical catalog reconciliation and read queries.
//!
//! Reconciliation converts activated provider streams into canonical channels
//! and binds those channels to active EPG channels. Read queries back the
//! control API with paginated, real data.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use iptv_domain::builtin_sports_event_rules;
use iptv_parsers::{
    CompiledEventRules, EventCandidate, EventGuideInput, GeneratedProgramme,
    GeneratedProgrammeKind, generate_event_schedule,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use std::{collections::BTreeMap, fmt, future::Future, pin::Pin};
use uuid::Uuid;

use crate::PersistenceError;

const DEFAULT_PAGE_SIZE: i64 = 100;
const MAX_PAGE_SIZE: i64 = 500;
/// Default canonical-key count for one reconciliation database batch.
pub const DEFAULT_RECONCILIATION_BATCH_SIZE: i64 = 500;

/// The fixed identity for the environment output profile.
pub const ENVIRONMENT_OUTPUT_PROFILE_ID: Uuid = Uuid::from_u128(1);

/// The fixed display name for the environment output profile.
pub const ENVIRONMENT_OUTPUT_PROFILE_NAME: &str = "Environment output";

/// A SHA-256 digest of an output token.
///
/// This type cannot contain a plaintext token. Its debug output does not
/// disclose the digest.
#[derive(Clone, Eq, PartialEq)]
pub struct OutputProfileTokenHash([u8; 32]);

impl OutputProfileTokenHash {
    pub fn from_sha256(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for OutputProfileTokenHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OutputProfileTokenHash(<redacted>)")
    }
}

/// A safe output profile view for public API use.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct OutputProfileRow {
    pub id: Uuid,
    pub name: String,
    pub tuner_count: i32,
    pub include_all_channels: bool,
    pub enabled: bool,
    pub revision: i64,
}

#[derive(Clone, Debug)]
pub struct CatalogRepository {
    pool: PgPool,
}

/// One durable provider-catalog reconciliation checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderReconciliationPhase {
    Snapshot,
    Channels,
    StreamLinks,
    Orphans,
    Revision,
}

/// Actual work counts from one provider-catalog reconciliation checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderReconciliationUpdate {
    pub phase: ProviderReconciliationPhase,
    /// Rows affected by the completed query phase.
    pub rows_affected: u64,
    /// Active supported provider streams that the reconciliation query reads.
    pub active_streams: u64,
    /// Canonical keys completed in the current reconciliation phase.
    pub keys_completed: u64,
    /// Canonical keys in the current reconciliation phase.
    pub keys_total: u64,
}

/// One durable parallel reconciliation run for a staged provider snapshot.
///
/// The run does not change public channels until its finalizer publishes it.
#[derive(Clone, Debug, FromRow, Serialize, PartialEq, Eq)]
pub struct ProviderReconciliationRun {
    pub id: Uuid,
    pub provider_account_id: Uuid,
    pub source_snapshot_id: Uuid,
    pub parent_job_id: Option<Uuid>,
    pub status: String,
    pub partition_count: i32,
    pub total_keys: i64,
    pub completed_keys: i64,
}

/// Aggregate durable progress for a parallel reconciliation run.
#[derive(Clone, Debug, FromRow, Serialize, PartialEq, Eq)]
pub struct ProviderReconciliationRunProgress {
    pub run_id: Uuid,
    pub status: String,
    pub partition_count: i64,
    pub partitions_completed: i64,
    pub total_keys: i64,
    pub completed_keys: i64,
}

/// One result of idempotent partition preparation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderReconciliationPartitionResult {
    pub run_id: Uuid,
    pub partition_number: i32,
    pub keys_completed: i64,
    pub total_keys: i64,
    pub already_completed: bool,
}

type ProviderReconciliationPartitionRow = (
    String,
    String,
    Uuid,
    i32,
    i64,
    String,
    Option<DateTime<Utc>>,
);

type ProviderReconciliationSnapshotRow =
    (String, String, Option<DateTime<Utc>>, DateTime<Utc>, i64);

/// Result from one attempt to publish all prepared reconciliation candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderReconciliationFinalization {
    /// One or more partitions still need work. Public output is unchanged.
    Pending(ProviderReconciliationRunProgress),
    /// The parent refresh was canceled before publication began.
    Cancelled,
    /// The staged snapshot and its catalog became public in one transaction.
    Published(ReconcileStats),
    /// A previous finalizer already published the run.
    AlreadyPublished,
    /// A newer provider snapshot already became public.
    Superseded,
}

/// Persists reconciliation checkpoints outside the catalog transaction.
///
/// A checkpoint error rolls back the catalog transaction. A checkpoint does
/// not publish partial catalog results.
pub trait ProviderReconciliationProgress: Send + Sync {
    #[allow(clippy::missing_errors_doc)]
    fn checkpoint(
        &self,
        update: ProviderReconciliationUpdate,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
}

struct NoopProviderReconciliationProgress;

impl ProviderReconciliationProgress for NoopProviderReconciliationProgress {
    fn checkpoint(
        &self,
        _update: ProviderReconciliationUpdate,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

impl CatalogRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates or returns durable work for one complete staged M3U or Xtream snapshot.
    ///
    /// A snapshot that is already active returns `None`. Workers can safely
    /// call this method again after a coordinator retry.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn create_or_load_provider_reconciliation_run(
        &self,
        snapshot_id: Uuid,
        parent_job_id: Uuid,
        requested_partitions: i32,
    ) -> Result<Option<ProviderReconciliationRun>, PersistenceError> {
        let partition_count = requested_partitions.clamp(1, 64);
        let mut transaction = self.pool.begin().await?;
        let snapshot: Option<(Uuid, String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
            r"
            SELECT provider_account_id, kind, status, staged_at
            FROM source_snapshots
            WHERE id = $1
            FOR UPDATE
            ",
        )
        .bind(snapshot_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((provider_account_id, kind, status, staged_at)) = snapshot else {
            return Err(PersistenceError::InvalidSource(
                "staged provider snapshot was not found".to_owned(),
            ));
        };
        if !matches!(kind.as_str(), "m3u" | "xtream") {
            return Err(PersistenceError::InvalidSource(
                "parallel reconciliation requires an M3U or Xtream snapshot".to_owned(),
            ));
        }
        if status == "active" {
            transaction.commit().await?;
            return Ok(None);
        }
        if status != "staging" || staged_at.is_none() {
            return Err(PersistenceError::InvalidSource(
                "provider snapshot is not ready for reconciliation".to_owned(),
            ));
        }

        if let Some(run) = sqlx::query_as::<_, ProviderReconciliationRun>(
            r"
            SELECT id, provider_account_id, source_snapshot_id, parent_job_id, status,
                   partition_count, total_keys, completed_keys
            FROM provider_reconciliation_runs
            WHERE source_snapshot_id = $1
            ORDER BY created_at DESC, id DESC
            LIMIT 1
            FOR UPDATE
            ",
        )
        .bind(snapshot_id)
        .fetch_optional(&mut *transaction)
        .await?
        {
            if run.status == "cancelled" {
                // Cancellation is terminal for this run. Keep its record for
                // audit, then create a new run for a later refresh.
            } else if run.status != "processing" {
                // A checksum-equivalent snapshot can become staging again
                // after a newer snapshot supersedes it. Rebuild its durable
                // run so terminal state does not strand the staged snapshot.
                sqlx::query(
                    "DELETE FROM provider_reconciliation_candidate_streams WHERE run_id = $1",
                )
                .bind(run.id)
                .execute(&mut *transaction)
                .await?;
                sqlx::query("DELETE FROM provider_reconciliation_candidates WHERE run_id = $1")
                    .bind(run.id)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(
                    r"
                    UPDATE provider_reconciliation_partitions
                    SET status = 'queued', completed_keys = 0, attempts = 0,
                        worker_id = NULL, completed_at = NULL, updated_at = now()
                    WHERE run_id = $1
                    ",
                )
                .bind(run.id)
                .execute(&mut *transaction)
                .await?;
                let run = sqlx::query_as::<_, ProviderReconciliationRun>(
                    r"
                    UPDATE provider_reconciliation_runs
                    SET status = 'processing', parent_job_id = $2, completed_keys = 0,
                        published_at = NULL, updated_at = now()
                    WHERE id = $1
                    RETURNING id, provider_account_id, source_snapshot_id, parent_job_id, status,
                              partition_count, total_keys, completed_keys
                    ",
                )
                .bind(run.id)
                .bind(parent_job_id)
                .fetch_one(&mut *transaction)
                .await?;
                crate::outbox::queue_reconciliation(&mut transaction, &run).await?;
                transaction.commit().await?;
                return Ok(Some(run));
            }
            if run.status != "cancelled" {
                crate::outbox::queue_reconciliation(&mut transaction, &run).await?;
                transaction.commit().await?;
                return Ok(Some(run));
            }
            // Canceled runs remain immutable audit records. The snapshot can
            // be reconciled again by inserting a new run below.
        }

        let total_keys: i64 = sqlx::query_scalar(
            r"
            SELECT count(*)
            FROM (
                SELECT COALESCE(NULLIF(tvg_id, ''), 'stream:' || stable_key)
                FROM provider_streams
                WHERE snapshot_id = $1 AND supported
                GROUP BY COALESCE(NULLIF(tvg_id, ''), 'stream:' || stable_key)
            ) AS canonical_keys
            ",
        )
        .bind(snapshot_id)
        .fetch_one(&mut *transaction)
        .await?;
        if total_keys == 0 {
            // Keep invalid provider data out of the staging state. A terminal
            // status also prevents repeated retries from stranding the same
            // checksum as an unpublishable snapshot.
            sqlx::query(
                "UPDATE source_snapshots SET status = 'rejected' WHERE id = $1 AND status = 'staging'",
            )
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Err(PersistenceError::InvalidSource(
                "staged provider snapshot contains no supported channels".to_owned(),
            ));
        }

        let run_id = Uuid::now_v7();
        let run = sqlx::query_as::<_, ProviderReconciliationRun>(
            r"
            INSERT INTO provider_reconciliation_runs
                (id, provider_account_id, source_snapshot_id, parent_job_id, status, partition_count, total_keys)
            VALUES ($1, $2, $3, $4, 'processing', $5, $6)
            RETURNING id, provider_account_id, source_snapshot_id, parent_job_id, status,
                      partition_count, total_keys, completed_keys
            ",
        )
        .bind(run_id)
        .bind(provider_account_id)
        .bind(snapshot_id)
        .bind(parent_job_id)
        .bind(partition_count)
        .bind(total_keys)
        .fetch_one(&mut *transaction)
        .await?;

        // Group canonical keys once, then distribute the counts across all
        // partitions. This keeps coordinator work O(streams), regardless of
        // the configured worker partition count.
        sqlx::query(
            r"
            WITH canonical_keys AS (
                SELECT COALESCE(NULLIF(tvg_id, ''), 'stream:' || stable_key) AS canonical_key
                FROM provider_streams
                WHERE snapshot_id = $1
                  AND supported
                GROUP BY canonical_key
            ), partition_counts AS (
                SELECT mod(
                           mod(hashtextextended(canonical_key, 0), $2::bigint) + $2::bigint,
                           $2::bigint
                       ) AS partition_number,
                       count(*) AS key_count
                FROM canonical_keys
                GROUP BY partition_number
            )
            INSERT INTO provider_reconciliation_partitions
                (run_id, partition_number, status, key_count)
            SELECT $3, partitions.partition_number, 'queued',
                   COALESCE(partition_counts.key_count, 0)
            FROM generate_series(0, $2::integer - 1) AS partitions(partition_number)
            LEFT JOIN partition_counts
                ON partition_counts.partition_number = partitions.partition_number
            ON CONFLICT (run_id, partition_number) DO NOTHING
            ",
        )
        .bind(snapshot_id)
        .bind(i64::from(partition_count))
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;
        crate::outbox::queue_reconciliation(&mut transaction, &run).await?;
        transaction.commit().await?;
        Ok(Some(run))
    }

    /// Returns aggregate partition progress from durable reconciliation state.
    #[allow(clippy::missing_errors_doc)]
    pub async fn provider_reconciliation_progress(
        &self,
        run_id: Uuid,
    ) -> Result<ProviderReconciliationRunProgress, PersistenceError> {
        provider_reconciliation_progress(&self.pool, run_id).await
    }

    /// Prepares one immutable snapshot partition without changing public output.
    ///
    /// The canonical key hash fixes one key to one partition. Repeated work is
    /// safe because candidate rows and candidate stream rows have stable keys.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn process_provider_reconciliation_partition(
        &self,
        run_id: Uuid,
        partition_number: i32,
        worker_id: &str,
    ) -> Result<ProviderReconciliationPartitionResult, PersistenceError> {
        self.process_provider_reconciliation_partition_inner(
            run_id,
            partition_number,
            worker_id,
            None,
        )
        .await
    }

    /// Processes a partition only while its claimed job remains owned by the worker.
    ///
    /// # Errors
    /// Returns an error when the database operation fails or the worker does not own the job.
    pub async fn process_provider_reconciliation_partition_owned(
        &self,
        run_id: Uuid,
        partition_number: i32,
        worker_id: &str,
        job_id: Uuid,
    ) -> Result<ProviderReconciliationPartitionResult, PersistenceError> {
        self.process_provider_reconciliation_partition_inner(
            run_id,
            partition_number,
            worker_id,
            Some(job_id),
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn process_provider_reconciliation_partition_inner(
        &self,
        run_id: Uuid,
        partition_number: i32,
        worker_id: &str,
        job_id: Option<Uuid>,
    ) -> Result<ProviderReconciliationPartitionResult, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '300s'")
            .execute(&mut *transaction)
            .await?;
        if let Some(job_id) = job_id {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM jobs WHERE id = $1 AND status = 'running' AND locked_by = $2 FOR UPDATE)",
            )
            .bind(job_id)
            .bind(worker_id)
            .fetch_one(&mut *transaction)
            .await?;
            if !owned {
                return Err(PersistenceError::JobOwnership {
                    job_id,
                    worker_id: worker_id.to_owned(),
                });
            }
        }
        let row: Option<ProviderReconciliationPartitionRow> = sqlx::query_as(
            r"
                SELECT p.status, r.status, r.source_snapshot_id, r.partition_count,
                       p.key_count, ss.status, ss.staged_at
                FROM provider_reconciliation_partitions p
                JOIN provider_reconciliation_runs r ON r.id = p.run_id
                JOIN source_snapshots ss ON ss.id = r.source_snapshot_id
                WHERE p.run_id = $1 AND p.partition_number = $2
                FOR UPDATE OF p
                ",
        )
        .bind(run_id)
        .bind(partition_number)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((
            partition_status,
            run_status,
            snapshot_id,
            partition_count,
            key_count,
            snapshot_status,
            staged_at,
        )) = row
        else {
            return Err(PersistenceError::InvalidSource(
                "reconciliation partition was not found".to_owned(),
            ));
        };
        if partition_status == "succeeded" {
            transaction.commit().await?;
            return Ok(ProviderReconciliationPartitionResult {
                run_id,
                partition_number,
                keys_completed: key_count,
                total_keys: key_count,
                already_completed: true,
            });
        }
        if run_status != "processing" || snapshot_status != "staging" || staged_at.is_none() {
            return Err(PersistenceError::InvalidSource(
                "reconciliation run is not ready for partition work".to_owned(),
            ));
        }

        sqlx::query(
            r"
            UPDATE provider_reconciliation_partitions
            SET status = 'processing', attempts = attempts + 1, worker_id = $3, updated_at = now()
            WHERE run_id = $1 AND partition_number = $2
            ",
        )
        .bind(run_id)
        .bind(partition_number)
        .bind(worker_id)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r"
            WITH grouped AS (
                SELECT COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) AS canonical_key,
                       (array_agg(ps.name ORDER BY ps.id))[1] AS name,
                       (array_agg(ps.group_name ORDER BY ps.id)
                            FILTER (WHERE ps.group_name IS NOT NULL))[1] AS group_name,
                       (array_agg(ps.logo_url ORDER BY ps.id)
                            FILTER (WHERE ps.logo_url IS NOT NULL AND ps.logo_url <> ''))[1] AS logo_url,
                       (array_agg(ps.channel_number ORDER BY ps.id)
                            FILTER (WHERE ps.channel_number IS NOT NULL AND ps.channel_number <> ''))[1] AS preferred_number
                FROM provider_streams ps
                WHERE ps.snapshot_id = $1
                  AND ps.supported
                  AND mod(
                      mod(hashtextextended(COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key), 0), $3::bigint)
                      + $3::bigint,
                      $3::bigint
                  ) = $2::bigint
                GROUP BY canonical_key
            )
            INSERT INTO provider_reconciliation_candidates
                (run_id, canonical_key, name, group_name, logo_url, preferred_number)
            SELECT $4, canonical_key, name, group_name, logo_url, preferred_number
            FROM grouped
            ON CONFLICT (run_id, canonical_key) DO UPDATE SET
                name = EXCLUDED.name,
                group_name = EXCLUDED.group_name,
                logo_url = EXCLUDED.logo_url,
                preferred_number = EXCLUDED.preferred_number
            ",
        )
        .bind(snapshot_id)
        .bind(i64::from(partition_number))
        .bind(i64::from(partition_count))
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r"
            INSERT INTO provider_reconciliation_candidate_streams
                (run_id, canonical_key, provider_stream_id)
            SELECT $4,
                   COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key),
                   ps.id
            FROM provider_streams ps
            WHERE ps.snapshot_id = $1
              AND ps.supported
              AND mod(
                  mod(hashtextextended(COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key), 0), $3::bigint)
                  + $3::bigint,
                  $3::bigint
              ) = $2::bigint
            ON CONFLICT DO NOTHING
            ",
        )
        .bind(snapshot_id)
        .bind(i64::from(partition_number))
        .bind(i64::from(partition_count))
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;

        let candidate_count: i64 = sqlx::query_scalar(
            r"
            SELECT count(*)
            FROM provider_reconciliation_candidates
            WHERE run_id = $1
              AND mod(
                  mod(hashtextextended(canonical_key, 0), $3::bigint) + $3::bigint,
                  $3::bigint
              ) = $2::bigint
            ",
        )
        .bind(run_id)
        .bind(i64::from(partition_number))
        .bind(i64::from(partition_count))
        .fetch_one(&mut *transaction)
        .await?;
        if candidate_count != key_count {
            return Err(PersistenceError::InvalidSource(
                "reconciliation candidate count does not match its partition".to_owned(),
            ));
        }
        sqlx::query(
            r"
            UPDATE provider_reconciliation_partitions
            SET status = 'succeeded', completed_keys = key_count, completed_at = now(), updated_at = now()
            WHERE run_id = $1 AND partition_number = $2
            ",
        )
        .bind(run_id)
        .bind(partition_number)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r"
            UPDATE provider_reconciliation_runs
            SET completed_keys = (
                    SELECT coalesce(sum(completed_keys), 0)
                    FROM provider_reconciliation_partitions
                    WHERE run_id = $1
                ),
                updated_at = now()
            WHERE id = $1
            ",
        )
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(ProviderReconciliationPartitionResult {
            run_id,
            partition_number,
            keys_completed: key_count,
            total_keys: key_count,
            already_completed: false,
        })
    }

    /// Publishes complete candidates and their staged snapshot in one transaction.
    ///
    /// The method returns `Pending` until every partition succeeds. It never
    /// makes one partition visible to playback clients.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn finalize_provider_reconciliation(
        &self,
        run_id: Uuid,
        parent_job_id: Uuid,
        finalizer_job_id: Uuid,
    ) -> Result<ProviderReconciliationFinalization, PersistenceError> {
        self.finalize_provider_reconciliation_inner(run_id, parent_job_id, finalizer_job_id, None)
            .await
    }

    /// Publishes only while the finalizer job remains owned by `worker_id`.
    ///
    /// # Errors
    /// Returns an error when the database operation fails or the worker does not own the job.
    pub async fn finalize_provider_reconciliation_owned(
        &self,
        run_id: Uuid,
        parent_job_id: Uuid,
        finalizer_job_id: Uuid,
        worker_id: &str,
    ) -> Result<ProviderReconciliationFinalization, PersistenceError> {
        self.finalize_provider_reconciliation_inner(
            run_id,
            parent_job_id,
            finalizer_job_id,
            Some(worker_id),
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn finalize_provider_reconciliation_inner(
        &self,
        run_id: Uuid,
        parent_job_id: Uuid,
        finalizer_job_id: Uuid,
        worker_id: Option<&str>,
    ) -> Result<ProviderReconciliationFinalization, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '300s'")
            .execute(&mut *transaction)
            .await?;
        let parent_status: Option<String> = sqlx::query_scalar(
            r"
            SELECT status
            FROM jobs
            WHERE id = $1 AND kind = 'refresh-source'
            FOR UPDATE
            ",
        )
        .bind(parent_job_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(parent_status) = parent_status else {
            return Err(PersistenceError::JobNotFound(parent_job_id));
        };
        // A finalizer is part of the parent refresh operation. Once the
        // parent reaches any terminal state, child work must stop and leave
        // the active catalog unchanged.
        if parent_status != "running" {
            sqlx::query(
                r"
                UPDATE jobs
                SET status = 'cancelled', completed_at = now(), updated_at = now(),
                    locked_by = NULL, locked_at = NULL, heartbeat_at = NULL
                WHERE id = $1
                  AND kind = 'finalize-provider-reconciliation'
                  AND status IN ('queued', 'running')
                ",
            )
            .bind(finalizer_job_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE provider_reconciliation_runs SET status = 'cancelled', updated_at = now() WHERE id = $1 AND status IN ('queued', 'processing')",
            )
            .bind(run_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query("DELETE FROM provider_reconciliation_candidate_streams WHERE run_id = $1")
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM provider_reconciliation_candidates WHERE run_id = $1")
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            return Ok(ProviderReconciliationFinalization::Cancelled);
        }
        let finalizer_status: Option<String> = sqlx::query_scalar(
            r"
            SELECT status
            FROM jobs
            WHERE id = $1 AND kind = 'finalize-provider-reconciliation'
              AND ($2::text IS NULL OR (status = 'running' AND locked_by = $2))
            FOR UPDATE
            ",
        )
        .bind(finalizer_job_id)
        .bind(worker_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(finalizer_status) = finalizer_status else {
            return Err(PersistenceError::JobNotFound(finalizer_job_id));
        };
        if finalizer_status == "cancelled" {
            transaction.commit().await?;
            return Ok(ProviderReconciliationFinalization::Cancelled);
        }
        // Lock the snapshot before the run. Run creation uses this same order
        // so a parent retry cannot deadlock with finalizer publication.
        let run_reference: Option<(Uuid, Uuid)> = sqlx::query_as(
            r"
            SELECT provider_account_id, source_snapshot_id
            FROM provider_reconciliation_runs
            WHERE id = $1
            ",
        )
        .bind(run_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((account_id, snapshot_id)) = run_reference else {
            return Err(PersistenceError::InvalidSource(
                "reconciliation run was not found".to_owned(),
            ));
        };
        sqlx::query(
            "SELECT 1 FROM source_snapshots WHERE id = $1 AND provider_account_id = $2 FOR UPDATE",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| {
            PersistenceError::InvalidSource("staged provider snapshot was not found".to_owned())
        })?;
        let run: Option<(Uuid, Uuid, String)> = sqlx::query_as(
            r"
            SELECT provider_account_id, source_snapshot_id, status
            FROM provider_reconciliation_runs
            WHERE id = $1
            FOR UPDATE
            ",
        )
        .bind(run_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((account_id, snapshot_id, status)) = run else {
            return Err(PersistenceError::InvalidSource(
                "reconciliation run was not found".to_owned(),
            ));
        };
        if status == "published" {
            transaction.commit().await?;
            return Ok(ProviderReconciliationFinalization::AlreadyPublished);
        }
        if status != "processing" {
            return Err(PersistenceError::InvalidSource(
                "reconciliation run is not ready for publication".to_owned(),
            ));
        }
        // Serialize publication for one provider account. Without this lock,
        // an older run can publish after a newer run and roll public data back.
        sqlx::query("SELECT 1 FROM provider_accounts WHERE id = $1 FOR UPDATE")
            .bind(account_id)
            .fetch_one(&mut *transaction)
            .await?;
        let unfinished: i64 = sqlx::query_scalar(
            r"
            SELECT count(*)
            FROM provider_reconciliation_partitions
            WHERE run_id = $1 AND status <> 'succeeded'
            ",
        )
        .bind(run_id)
        .fetch_one(&mut *transaction)
        .await?;
        if unfinished > 0 {
            transaction.commit().await?;
            return Ok(ProviderReconciliationFinalization::Pending(
                self.provider_reconciliation_progress(run_id).await?,
            ));
        }
        // Serialize cancellation with publication. The lock is set only after
        // all partitions finish, so cancellation remains available while work
        // is still pending and wins before this transaction acquires the row.
        sqlx::query(
            r"
            UPDATE jobs
            SET progress = progress || jsonb_build_object('activationLocked', true),
                updated_at = now()
            WHERE id = $1 AND status <> 'cancelled'
            ",
        )
        .bind(parent_job_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r"
            UPDATE jobs
            SET progress = progress || jsonb_build_object('activationLocked', true),
                updated_at = now()
            WHERE id = $1 AND status <> 'cancelled'
            ",
        )
        .bind(finalizer_job_id)
        .execute(&mut *transaction)
        .await?;
        let snapshot: Option<ProviderReconciliationSnapshotRow> = sqlx::query_as(
            r"
            SELECT kind, status, staged_at, fetched_at, record_count
            FROM source_snapshots
            WHERE id = $1 AND provider_account_id = $2
            FOR UPDATE
            ",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((kind, snapshot_status, staged_at, fetched_at, record_count)) = snapshot else {
            return Err(PersistenceError::InvalidSource(
                "staged provider snapshot was not found".to_owned(),
            ));
        };
        if !matches!(kind.as_str(), "m3u" | "xtream")
            || snapshot_status != "staging"
            || staged_at.is_none()
        {
            return Err(PersistenceError::InvalidSource(
                "staged provider snapshot is not ready for publication".to_owned(),
            ));
        }
        let newer_active: Option<Uuid> = sqlx::query_scalar(
            r"
            SELECT id
            FROM source_snapshots
            WHERE provider_account_id = $1
              AND kind = $2
              AND status = 'active'
              AND (fetched_at > $3 OR (fetched_at = $3 AND id > $4))
            LIMIT 1
            ",
        )
        .bind(account_id)
        .bind(&kind)
        .bind(fetched_at)
        .bind(snapshot_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if newer_active.is_some() {
            sqlx::query(
                "UPDATE source_snapshots SET status = 'superseded' WHERE id = $1 AND status = 'staging'",
            )
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE provider_reconciliation_runs SET status = 'cancelled', updated_at = now() WHERE id = $1 AND status = 'processing'",
            )
            .bind(run_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query("DELETE FROM provider_reconciliation_candidate_streams WHERE run_id = $1")
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM provider_reconciliation_candidates WHERE run_id = $1")
                .bind(run_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            return Ok(ProviderReconciliationFinalization::Superseded);
        }
        let stream_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM provider_streams WHERE snapshot_id = $1")
                .bind(snapshot_id)
                .fetch_one(&mut *transaction)
                .await?;
        if stream_count != record_count {
            return Err(PersistenceError::InvalidSource(
                "staged provider snapshot record count does not match its streams".to_owned(),
            ));
        }
        let candidate_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM provider_reconciliation_candidates WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&mut *transaction)
        .await?;
        let total_keys: i64 =
            sqlx::query_scalar("SELECT total_keys FROM provider_reconciliation_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&mut *transaction)
                .await?;
        if candidate_count != total_keys {
            return Err(PersistenceError::InvalidSource(
                "reconciliation candidates are incomplete".to_owned(),
            ));
        }

        let before_snapshot = capture_reconciliation_snapshot(&mut transaction, account_id).await?;
        let current_revision =
            current_reconciliation_revision(&mut transaction, account_id).await?;
        let channel_count = upsert_candidate_channels(&mut transaction, account_id, run_id).await?;
        let stale_link_count =
            remove_stale_candidate_channel_stream_links(&mut transaction, account_id, run_id)
                .await?;
        let linked_count =
            upsert_candidate_channel_stream_links(&mut transaction, account_id, run_id).await?;
        let orphans_removed =
            remove_candidate_orphaned_channels(&mut transaction, account_id, run_id).await?;

        sqlx::query(
            r"
            UPDATE source_snapshots
            SET status = 'superseded'
            WHERE provider_account_id = $1
              AND kind IN ('m3u', 'xtream')
              AND status = 'active'
              AND id <> $2
            ",
        )
        .bind(account_id)
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await?;
        let activated = sqlx::query(
            r"
            UPDATE source_snapshots
            SET status = 'active', activated_at = now()
            WHERE id = $1 AND status = 'staging'
            ",
        )
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await?;
        if activated.rows_affected() != 1 {
            return Err(PersistenceError::InvalidSource(
                "staged provider snapshot was not activatable".to_owned(),
            ));
        }
        let after_snapshot = capture_reconciliation_snapshot(&mut transaction, account_id).await?;
        record_reconciliation_revision(
            &mut transaction,
            account_id,
            current_revision + 1,
            "system",
            before_snapshot,
            after_snapshot,
        )
        .await?;
        // Publication and revision capture have consumed the candidates. Drop
        // both candidate tables before the run becomes terminal so retries
        // cannot observe half-published reconciliation state.
        sqlx::query("DELETE FROM provider_reconciliation_candidate_streams WHERE run_id = $1")
            .bind(run_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM provider_reconciliation_candidates WHERE run_id = $1")
            .bind(run_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            r"
            UPDATE provider_reconciliation_runs
            SET status = 'published', completed_keys = total_keys,
                published_at = now(), updated_at = now()
            WHERE id = $1 AND status = 'processing'
            ",
        )
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(ProviderReconciliationFinalization::Published(
            ReconcileStats {
                channels: channel_count,
                orphaned_channels_removed: orphans_removed,
                stream_links: stale_link_count.saturating_add(linked_count),
            },
        ))
    }

    /// Reconciles active provider snapshots into canonical channels and
    /// channel-stream links. The snapshots can have the M3U or Xtream kind.
    /// Streams that share a non-blank
    /// `tvg_id` merge into one channel; streams without a `tvg_id` become their
    /// own channel. Prior automatic channels for the account are replaced.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any reconcile query fails.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn reconcile_provider_account(
        &self,
        account_id: Uuid,
    ) -> Result<ReconcileStats, PersistenceError> {
        self.reconcile_provider_account_with_progress(
            account_id,
            &NoopProviderReconciliationProgress,
        )
        .await
    }

    /// Reconciles one provider account and reports completed database query phases.
    ///
    /// Each update includes actual rows affected and the active-stream count.
    /// The transaction remains atomic when a progress write fails.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn reconcile_provider_account_with_progress(
        &self,
        account_id: Uuid,
        progress: &dyn ProviderReconciliationProgress,
    ) -> Result<ReconcileStats, PersistenceError> {
        self.reconcile_provider_account_with_progress_and_batch(
            account_id,
            progress,
            DEFAULT_RECONCILIATION_BATCH_SIZE,
        )
        .await
    }

    /// Reconciles one provider account in deterministic canonical-key batches.
    ///
    /// One transaction contains every batch. A failed batch or checkpoint
    /// rolls back all catalog changes.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn reconcile_provider_account_with_progress_and_batch(
        &self,
        account_id: Uuid,
        progress: &dyn ProviderReconciliationProgress,
        batch_size: i64,
    ) -> Result<ReconcileStats, PersistenceError> {
        let batch_size = batch_size.clamp(1, 10_000);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '300s'")
            .execute(&mut *transaction)
            .await?;
        let active_streams = active_supported_stream_count(&mut transaction, account_id).await?;
        let canonical_key_total = active_canonical_key_count(&mut transaction, account_id).await?;
        // Capture the automatic channel, stream link, and EPG mapping state
        // for this account before reconciliation overwrites it. The snapshot
        // is stored in the shared revisions table so a later rollback can
        // restore channels, streams, and EPG mappings together.
        let before_snapshot = capture_reconciliation_snapshot(&mut transaction, account_id).await?;
        progress
            .checkpoint(ProviderReconciliationUpdate {
                phase: ProviderReconciliationPhase::Snapshot,
                rows_affected: 0,
                active_streams,
                keys_completed: 0,
                keys_total: canonical_key_total,
            })
            .await?;
        let current_revision =
            current_reconciliation_revision(&mut transaction, account_id).await?;
        let (channel_count, _) = reconcile_canonical_key_batches(
            &mut transaction,
            account_id,
            batch_size,
            canonical_key_total,
            active_streams,
            progress,
            ProviderReconciliationPhase::Channels,
        )
        .await?;
        let stale_link_count =
            remove_stale_channel_stream_links(&mut transaction, account_id).await?;
        let (linked_count, _) = reconcile_canonical_key_batches(
            &mut transaction,
            account_id,
            batch_size,
            canonical_key_total,
            active_streams,
            progress,
            ProviderReconciliationPhase::StreamLinks,
        )
        .await?;
        let link_count = stale_link_count.saturating_add(linked_count);
        let orphans_removed = remove_orphaned_channels(&mut transaction, account_id).await?;
        progress
            .checkpoint(ProviderReconciliationUpdate {
                phase: ProviderReconciliationPhase::Orphans,
                rows_affected: u64::try_from(orphans_removed.max(0)).unwrap_or(u64::MAX),
                active_streams,
                keys_completed: canonical_key_total,
                keys_total: canonical_key_total,
            })
            .await?;
        let after_snapshot = capture_reconciliation_snapshot(&mut transaction, account_id).await?;
        record_reconciliation_revision(
            &mut transaction,
            account_id,
            current_revision + 1,
            "system",
            before_snapshot,
            after_snapshot,
        )
        .await?;
        progress
            .checkpoint(ProviderReconciliationUpdate {
                phase: ProviderReconciliationPhase::Revision,
                rows_affected: 1,
                active_streams,
                keys_completed: canonical_key_total,
                keys_total: canonical_key_total,
            })
            .await?;
        transaction.commit().await?;

        Ok(ReconcileStats {
            channels: channel_count,
            orphaned_channels_removed: orphans_removed,
            stream_links: link_count,
        })
    }

    /// Rebuilds EPG mappings for automatic canonical channels.
    ///
    /// Matching applies in priority order:
    /// 1. Exact case-insensitive `tvg-id` match (confidence 0.99).
    /// 2. Normalized name match that strips quality, resolution, and
    ///    country-prefix tokens from both channel and EPG display names
    ///    (confidence 0.90).
    /// 3. Channel alias match: when a channel name and an EPG display name
    ///    resolve to the same canonical name via the `channel_aliases` table,
    ///    a mapping is created with confidence 0.85.
    /// 4. Ambiguous name matches populate `review_candidates` for operator
    ///    review rather than auto-applying the first result.
    ///
    /// Manual mappings (`review_status = 'manual'`) are preserved and never
    /// overwritten by automatic reconciliation.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any mapping query fails.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn reconcile_epg_mappings(&self) -> Result<EpgMappingStats, PersistenceError> {
        let mut transaction = self.pool.begin().await?;

        // Clear old review candidates before re-evaluation.
        sqlx::query("DELETE FROM review_candidates")
            .execute(&mut *transaction)
            .await?;

        // Pass 1: exact tvg-id match (highest confidence).
        // Skips channels that already have a manual mapping.
        let tvg_applied: i64 = sqlx::query(
            r"
            WITH active_epg_channels AS (
                SELECT DISTINCT ON (lower(ec.xmltv_id)) ec.id, lower(ec.xmltv_id) AS xmltv_id
                FROM epg_channels ec
                JOIN source_snapshots ss
                  ON ss.id = ec.source_snapshot_id
                 AND ss.status = 'active'
                ORDER BY lower(ec.xmltv_id), ss.activated_at DESC NULLS LAST, ec.id DESC
            )
            INSERT INTO channel_epg_mappings
                (channel_id, epg_channel_id, method, confidence, evidence,
                 review_status, revision)
            SELECT c.id, aec.id, 'tvg-id', 0.99,
                   jsonb_build_object('match', 'tvg-id-exact'), 'applied', 1
            FROM channels c
            JOIN active_epg_channels aec
              ON aec.xmltv_id = lower(c.canonical_key)
            WHERE c.canonical_key IS NOT NULL
              AND c.canonical_key NOT LIKE 'stream:%'
              AND NOT EXISTS (
                SELECT 1 FROM channel_epg_mappings cem
                WHERE cem.channel_id = c.id AND cem.review_status = 'manual'
              )
            ON CONFLICT (channel_id) DO UPDATE SET
                epg_channel_id = EXCLUDED.epg_channel_id,
                method = EXCLUDED.method,
                confidence = EXCLUDED.confidence,
                evidence = EXCLUDED.evidence,
                review_status = EXCLUDED.review_status,
                revision = channel_epg_mappings.revision + 1,
                updated_at = now()
            WHERE channel_epg_mappings.review_status <> 'manual'
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        // Pass 2: normalized name match for channels without a tvg-id mapping.
        // Strips country prefixes (e.g. "UK: ", "ES| "), quality tokens
        // (e.g. "[1080p]", "FHD", "HD"), and punctuation, then matches on
        // the remaining core name tokens.
        //
        // Only applies when exactly one EPG channel (deduplicated across
        // active snapshots) shares the normalized name. Ambiguous matches
        // populate review_candidates. VOD content is excluded.
        let name_applied: i64 = sqlx::query(
            r"
            WITH latest_active_xmltv AS (
                SELECT id AS snapshot_id
                FROM source_snapshots
                WHERE kind = 'xmltv' AND status = 'active'
                ORDER BY activated_at DESC NULLS LAST
                LIMIT 1
            ),
            epg_names AS (
                SELECT ec.id AS epg_channel_id,
                       normalize_channel_name(ec.display_names->0->>'value') AS norm
                FROM epg_channels ec
                JOIN latest_active_xmltv lax ON lax.snapshot_id = ec.source_snapshot_id
                WHERE normalize_channel_name(ec.display_names->0->>'value') <> ''
            ),
            unique_epg_names AS (
                SELECT norm,
                       (array_agg(epg_channel_id ORDER BY epg_channel_id))[1] AS epg_channel_id
                FROM epg_names
                GROUP BY norm
                HAVING count(*) = 1
            ),
            matchable_channels AS (
                SELECT c.id AS channel_id,
                       normalize_channel_name(c.name) AS norm
                FROM channels c
                WHERE c.managed_by = 'automatic'
                  AND c.canonical_key IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM channel_epg_mappings cem WHERE cem.channel_id = c.id
                  )
                  AND normalize_channel_name(c.name) <> ''
                  AND normalize_channel_name(c.name) NOT SIMILAR TO '%(s[0-9]+ e[0-9]+|[0-9]{4})%'
            )
            INSERT INTO channel_epg_mappings
                (channel_id, epg_channel_id, method, confidence, evidence,
                 review_status, revision)
            SELECT mc.channel_id, uen.epg_channel_id, 'exact-name', 0.90,
                   jsonb_build_object('match', 'normalized-name',
                                      'channel_name', mc.norm,
                                      'epg_name', uen.norm), 'applied', 1
            FROM matchable_channels mc
            JOIN unique_epg_names uen ON uen.norm = mc.norm
            ON CONFLICT (channel_id) DO UPDATE SET
                epg_channel_id = EXCLUDED.epg_channel_id,
                method = EXCLUDED.method,
                confidence = EXCLUDED.confidence,
                evidence = EXCLUDED.evidence,
                review_status = EXCLUDED.review_status,
                revision = channel_epg_mappings.revision + 1,
                updated_at = now()
            WHERE channel_epg_mappings.review_status <> 'manual'
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        // Pass 3: channel alias match for channels without a mapping.
        // When a channel name and an EPG display name resolve to the same
        // canonical name via the channel_aliases table, create a mapping.
        // This catches cases like "ESPN HD" (channel) -> "ESPN" (EPG)
        // where normalized name matching fails due to quality tokens.
        let alias_applied: i64 = sqlx::query(
            r"
            WITH latest_active_xmltv AS (
                SELECT id AS snapshot_id
                FROM source_snapshots
                WHERE kind = 'xmltv' AND status = 'active'
                ORDER BY activated_at DESC NULLS LAST
                LIMIT 1
            ),
            epg_names AS (
                SELECT ec.id AS epg_channel_id,
                       normalize_channel_name(ec.display_names->0->>'value') AS norm
                FROM epg_channels ec
                JOIN latest_active_xmltv lax ON lax.snapshot_id = ec.source_snapshot_id
                WHERE normalize_channel_name(ec.display_names->0->>'value') <> ''
            ),
            unique_epg_names AS (
                SELECT norm,
                       (array_agg(epg_channel_id ORDER BY epg_channel_id))[1] AS epg_channel_id
                FROM epg_names
                GROUP BY norm
                HAVING count(*) = 1
            ),
            alias_matchable AS (
                SELECT c.id AS channel_id,
                       normalize_channel_name(c.name) AS channel_norm,
                       ca.canonical_name AS canonical
                FROM channels c
                JOIN channel_aliases ca
                  ON normalize_channel_name(ca.alias) = normalize_channel_name(c.name)
                WHERE c.managed_by = 'automatic'
                  AND c.canonical_key IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM channel_epg_mappings cem WHERE cem.channel_id = c.id
                  )
                  AND normalize_channel_name(c.name) <> ''
            )
            INSERT INTO channel_epg_mappings
                (channel_id, epg_channel_id, method, confidence, evidence,
                 review_status, revision)
            SELECT am.channel_id, uen.epg_channel_id, 'alias', 0.85,
                   jsonb_build_object('match', 'channel-alias',
                                      'channel_name', am.channel_norm,
                                      'canonical_name', am.canonical,
                                      'epg_name', uen.norm), 'applied', 1
            FROM alias_matchable am
            JOIN unique_epg_names uen ON uen.norm = normalize_channel_name(am.canonical)
            ON CONFLICT (channel_id) DO UPDATE SET
                epg_channel_id = EXCLUDED.epg_channel_id,
                method = EXCLUDED.method,
                confidence = EXCLUDED.confidence,
                evidence = EXCLUDED.evidence,
                review_status = EXCLUDED.review_status,
                revision = channel_epg_mappings.revision + 1,
                updated_at = now()
            WHERE channel_epg_mappings.review_status <> 'manual'
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        // Pass 4: ambiguous name matches — store all candidates for review.
        // Channels whose normalized name matches multiple EPG channels get
        // review_candidate rows. If the channel has no mapping yet, create a
        // mapping with review_status = 'review' pointing at the top candidate.
        let review_queued: i64 = sqlx::query(
            r"
            WITH latest_active_xmltv AS (
                SELECT id AS snapshot_id
                FROM source_snapshots
                WHERE kind = 'xmltv' AND status = 'active'
                ORDER BY activated_at DESC NULLS LAST
                LIMIT 1
            ),
            epg_names AS (
                SELECT ec.id AS epg_channel_id,
                       normalize_channel_name(ec.display_names->0->>'value') AS norm
                FROM epg_channels ec
                JOIN latest_active_xmltv lax ON lax.snapshot_id = ec.source_snapshot_id
                WHERE normalize_channel_name(ec.display_names->0->>'value') <> ''
            ),
            ambiguous_epg_names AS (
                SELECT norm,
                       array_agg(epg_channel_id ORDER BY epg_channel_id) AS epg_ids
                FROM epg_names
                GROUP BY norm
                HAVING count(*) > 1
            ),
            ambiguous_channels AS (
                SELECT c.id AS channel_id,
                       normalize_channel_name(c.name) AS norm
                FROM channels c
                WHERE c.managed_by = 'automatic'
                  AND c.canonical_key IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM channel_epg_mappings cem WHERE cem.channel_id = c.id
                  )
                  AND normalize_channel_name(c.name) <> ''
                  AND normalize_channel_name(c.name) NOT SIMILAR TO '%(s[0-9]+ e[0-9]+|[0-9]{4})%'
            )
            INSERT INTO review_candidates
                (id, channel_id, epg_channel_id, method, confidence, evidence)
            SELECT gen_random_uuid(), ac.channel_id, epg_id, 'exact-name', 0.85,
                   jsonb_build_object('match', 'ambiguous-normalized-name',
                                      'channel_name', ac.norm)
            FROM ambiguous_channels ac
            JOIN ambiguous_epg_names aen ON aen.norm = ac.norm
            CROSS JOIN LATERAL unnest(aen.epg_ids) AS epg_id
            ON CONFLICT (channel_id, epg_channel_id) DO NOTHING
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        // For channels with review candidates but no mapping, create a
        // review-status mapping pointing at the top candidate.
        sqlx::query(
            r"
            INSERT INTO channel_epg_mappings
                (channel_id, epg_channel_id, method, confidence, evidence,
                 review_status, revision)
            SELECT DISTINCT ON (rc.channel_id)
                   rc.channel_id, rc.epg_channel_id, rc.method, rc.confidence,
                   rc.evidence, 'review', 1
            FROM review_candidates rc
            WHERE NOT EXISTS (
                SELECT 1 FROM channel_epg_mappings cem WHERE cem.channel_id = rc.channel_id
            )
            ORDER BY rc.channel_id, rc.confidence DESC, rc.epg_channel_id ASC
            ON CONFLICT (channel_id) DO NOTHING
            ",
        )
        .execute(&mut *transaction)
        .await?;

        // Remove automatic mappings whose target EPG channel is no longer an
        // active match. Each mapping stores its selected EPG channel ID, so
        // evaluate identity against that one row instead of scanning every
        // active EPG channel and re-running the name normalizer for every
        // mapping. A replacement XMLTV snapshot gives its channels new IDs,
        // which makes this check precise and bounded. Manual mappings remain
        // preserved.
        let removed: i64 = sqlx::query(
            r"
            DELETE FROM channel_epg_mappings cem
            USING channels c
            WHERE cem.channel_id = c.id
              AND c.managed_by = 'automatic'
              AND c.canonical_key IS NOT NULL
              AND cem.review_status <> 'manual'
              AND NOT EXISTS (
                SELECT 1
                FROM epg_channels ec
                JOIN source_snapshots ss ON ss.id = ec.source_snapshot_id
                WHERE ec.id = cem.epg_channel_id
                  AND ss.status = 'active'
                  AND (
                    lower(ec.xmltv_id) = lower(c.canonical_key)
                    OR normalize_channel_name(ec.display_names->0->>'value')
                      = normalize_channel_name(c.name)
                    OR EXISTS (
                      SELECT 1
                      FROM channel_aliases ca
                      WHERE normalize_channel_name(ca.alias)
                              = normalize_channel_name(c.name)
                        AND normalize_channel_name(ca.canonical_name)
                              = normalize_channel_name(ec.display_names->0->>'value')
                    )
                  )
              )
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);
        transaction.commit().await?;
        Ok(EpgMappingStats {
            mappings_applied: tvg_applied + name_applied + alias_applied,
            mappings_removed: removed,
            review_queued,
        })
    }

    /// Lists reconciliation revision history for one provider account, newest
    /// first. Each revision records the automatic channel, stream link, and
    /// EPG mapping snapshot captured before and after a reconciliation run.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_reconciliation_revisions(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<ReconciliationRevisionRow>, PersistenceError> {
        let rows: Vec<ReconciliationRevisionRow> = sqlx::query_as(
            r"
            SELECT revision, actor, before_value, after_value, created_at
            FROM revisions
            WHERE resource_type = 'reconciliation' AND resource_id = $1
            ORDER BY revision DESC
            ",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Restores the automatic channels, stream links, and EPG mappings for
    /// one provider account to the state captured at `target_revision`. A
    /// new revision row records the restore so the rollback itself is
    /// auditable and reversible.
    ///
    /// Manual channels and manual EPG mappings on channels outside the
    /// account's automatic set are preserved. The snapshot captured by
    /// [`CatalogRepository::reconcile_provider_account`] includes any manual
    /// EPG mappings attached to the account's automatic channels, so those
    /// are restored alongside the automatic mappings.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::ReconciliationRevisionNotFound`] when the
    /// target revision does not exist for the account.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn rollback_reconciliation(
        &self,
        account_id: Uuid,
        target_revision: i64,
        actor: &str,
    ) -> Result<ReconciliationRollbackStats, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '300s'")
            .execute(&mut *transaction)
            .await?;

        let target: Option<(Option<Value>, Value)> = sqlx::query_as(
            r"
            SELECT before_value, after_value
            FROM revisions
            WHERE resource_type = 'reconciliation' AND resource_id = $1 AND revision = $2
            ",
        )
        .bind(account_id)
        .bind(target_revision)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((_, after_value)) = target else {
            transaction.rollback().await.ok();
            return Err(PersistenceError::ReconciliationRevisionNotFound {
                account_id,
                target: target_revision,
            });
        };

        let snapshot: ReconciliationSnapshot =
            serde_json::from_value(after_value).map_err(|_| {
                PersistenceError::Database(sqlx::Error::Protocol(
                    "decode reconciliation snapshot".to_owned(),
                ))
            })?;

        // Capture the current state before any mutation. The rollback itself
        // creates a new revision, so this value must describe the state that
        // the rollback replaces. Capturing it after the restore would make
        // the audit record claim that both sides of the transition are the
        // restored state and would make the rollback history non-reversible.
        let before_snapshot = capture_reconciliation_snapshot(&mut transaction, account_id).await?;

        let target_channel_ids: Vec<Uuid> =
            snapshot.channels.iter().map(|channel| channel.id).collect();

        // A channel has dependents outside the reconciliation set. Deleting
        // it would cascade-delete recordings, rules, generated programmes,
        // profile assignments, user grants, review candidates, and event
        // links. Detach such channels instead. This preserves the dependent
        // rows while removing the channel from the automatic provider set.
        let preserved_channel_ids: Vec<Uuid> = sqlx::query_scalar(
            r"
            SELECT c.id
            FROM channels c
            WHERE c.provider_account_id = $1
              AND c.managed_by = 'automatic'
              AND NOT (c.id = ANY($2))
              AND (
                  EXISTS (SELECT 1 FROM output_profile_channels opc WHERE opc.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM user_channel_grants ucg WHERE ucg.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM recording_rules rr WHERE rr.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM recordings r WHERE r.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM channel_stream_profiles csp WHERE csp.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM generated_programmes gp WHERE gp.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM event_channels ec WHERE ec.channel_id = c.id)
                  OR EXISTS (SELECT 1 FROM review_candidates rc WHERE rc.channel_id = c.id)
              )
            ORDER BY c.id
            ",
        )
        .bind(account_id)
        .bind(&target_channel_ids)
        .fetch_all(&mut *transaction)
        .await?;

        if !preserved_channel_ids.is_empty() {
            sqlx::query(
                r"
                UPDATE channels
                SET enabled = false,
                    managed_by = 'manual',
                    provider_account_id = NULL,
                    canonical_key = NULL,
                    updated_at = now(),
                    revision = revision + 1
                WHERE id = ANY($1)
                ",
            )
            .bind(&preserved_channel_ids)
            .execute(&mut *transaction)
            .await?;
        }

        // Delete only channels with no external dependents. Their
        // reconciliation-owned stream links and EPG mappings can be replaced
        // safely below. Channels for other accounts remain untouched.
        let channels_to_remove: Vec<Uuid> = sqlx::query_scalar(
            r"
            SELECT c.id
            FROM channels c
            WHERE c.provider_account_id = $1
              AND c.managed_by = 'automatic'
              AND NOT (c.id = ANY($2))
              AND NOT (c.id = ANY($3))
            ORDER BY c.id
            ",
        )
        .bind(account_id)
        .bind(&target_channel_ids)
        .bind(&preserved_channel_ids)
        .fetch_all(&mut *transaction)
        .await?;
        let channels_removed: i64 = if channels_to_remove.is_empty() {
            0
        } else {
            sqlx::query("DELETE FROM channels WHERE id = ANY($1)")
                .bind(&channels_to_remove)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
                .try_into()
                .unwrap_or(i64::MAX)
        };

        // Replace links and mappings for the target channel set. Keep the
        // rows for preserved channels because they remain valid manual data.
        if !target_channel_ids.is_empty() {
            sqlx::query("DELETE FROM channel_streams WHERE channel_id = ANY($1)")
                .bind(&target_channel_ids)
                .execute(&mut *transaction)
                .await?;
            sqlx::query("DELETE FROM channel_epg_mappings WHERE channel_id = ANY($1)")
                .bind(&target_channel_ids)
                .execute(&mut *transaction)
                .await?;
        }

        // Re-create the snapshot channels with their original ids. When the
        // original channel number is now taken by a non-account channel, fall
        // back to the canonical channel number sequence so the unique
        // constraint stays satisfied.
        let mut channels_restored: i64 = 0;
        for channel in &snapshot.channels {
            let channel_number = if channel_number_available_for(
                &mut transaction,
                &channel.channel_number,
                channel.id,
            )
            .await?
            {
                channel.channel_number.clone()
            } else {
                next_channel_number(&mut transaction).await?
            };
            sqlx::query(
                r"
                INSERT INTO channels
                    (id, channel_number, name, group_name, logo_url, enabled,
                     managed_by, provider_account_id, canonical_key, revision)
                VALUES ($1, $2, $3, $4, $5, $6, 'automatic', $7, $8, 1)
                ON CONFLICT (id) DO UPDATE SET
                    channel_number = EXCLUDED.channel_number,
                    name = EXCLUDED.name,
                    group_name = EXCLUDED.group_name,
                    logo_url = EXCLUDED.logo_url,
                    enabled = EXCLUDED.enabled,
                    managed_by = EXCLUDED.managed_by,
                    provider_account_id = EXCLUDED.provider_account_id,
                    canonical_key = EXCLUDED.canonical_key,
                    updated_at = now()
                ",
            )
            .bind(channel.id)
            .bind(&channel_number)
            .bind(&channel.name)
            .bind(&channel.group_name)
            .bind(&channel.logo_url)
            .bind(channel.enabled)
            .bind(account_id)
            .bind(&channel.canonical_key)
            .execute(&mut *transaction)
            .await?;
            channels_restored += 1;
        }

        // Restore stream links for the re-created channels.
        let mut stream_links_restored: i64 = 0;
        for link in &snapshot.stream_links {
            sqlx::query(
                r"
                INSERT INTO channel_streams (channel_id, provider_stream_id, priority, evidence)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (channel_id, provider_stream_id) DO UPDATE SET
                    priority = EXCLUDED.priority,
                    evidence = EXCLUDED.evidence
                ",
            )
            .bind(link.channel_id)
            .bind(link.provider_stream_id)
            .bind(link.priority)
            .bind(&link.evidence)
            .execute(&mut *transaction)
            .await?;
            stream_links_restored += 1;
        }

        // Restore EPG mappings, including manual bindings that were attached
        // to the account's automatic channels at the snapshot revision.
        let mut epg_mappings_restored: i64 = 0;
        for mapping in &snapshot.epg_mappings {
            sqlx::query(
                r"
                INSERT INTO channel_epg_mappings
                    (channel_id, epg_channel_id, method, confidence, evidence,
                     review_status, reviewed_by, reviewed_at, revision)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1)
                ON CONFLICT (channel_id) DO UPDATE SET
                    epg_channel_id = EXCLUDED.epg_channel_id,
                    method = EXCLUDED.method,
                    confidence = EXCLUDED.confidence,
                    evidence = EXCLUDED.evidence,
                    review_status = EXCLUDED.review_status,
                    reviewed_by = EXCLUDED.reviewed_by,
                    reviewed_at = EXCLUDED.reviewed_at,
                    revision = channel_epg_mappings.revision + 1,
                    updated_at = now()
                ",
            )
            .bind(mapping.channel_id)
            .bind(mapping.epg_channel_id)
            .bind(&mapping.method)
            .bind(mapping.confidence)
            .bind(&mapping.evidence)
            .bind(&mapping.review_status)
            .bind(&mapping.reviewed_by)
            .bind(mapping.reviewed_at)
            .execute(&mut *transaction)
            .await?;
            epg_mappings_restored += 1;
        }

        let current_revision =
            current_reconciliation_revision(&mut transaction, account_id).await?;
        let after_snapshot = capture_reconciliation_snapshot(&mut transaction, account_id).await?;
        record_reconciliation_revision(
            &mut transaction,
            account_id,
            current_revision + 1,
            actor,
            before_snapshot,
            after_snapshot,
        )
        .await?;

        transaction.commit().await?;
        Ok(ReconciliationRollbackStats {
            target_revision,
            channels_removed,
            channels_restored,
            stream_links_restored,
            epg_mappings_restored,
        })
    }

    /// Lists canonical channels with server-side pagination and search.
    ///
    /// Uses a deferred join: first find page IDs via the `channel_number` index,
    /// then fetch full details and stream counts only for those rows. This
    /// avoids scanning and grouping all rows when paginating large catalogs.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_channels(
        &self,
        query: ChannelQuery,
    ) -> Result<ChannelPage, PersistenceError> {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let offset = query.offset.unwrap_or(0).max(0);
        let pattern = query.search.as_deref().map(|value| {
            format!(
                "%{}%",
                value
                    .trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
        let group_filter = query
            .group
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let total: i64 = sqlx::query_scalar(
            r"
            SELECT count(*) FROM channels
            WHERE ($1::text IS NULL
                   OR lower(name) LIKE lower($1) ESCAPE '\'
                   OR lower(coalesce(group_name, '')) LIKE lower($1) ESCAPE '\')
              AND ($2::text IS NULL OR lower(coalesce(group_name, '')) = lower($2))
              AND ($5::bool IS NULL OR enabled = $5)
            ",
        )
        .bind(pattern.as_deref())
        .bind(group_filter)
        .bind(limit)
        .bind(offset)
        .bind(query.enabled)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, ChannelRow>(
            r"
            WITH page_ids AS (
                SELECT id
                FROM channels
                WHERE ($1::text IS NULL
                       OR lower(name) LIKE lower($1) ESCAPE '\'
                       OR lower(coalesce(group_name, '')) LIKE lower($1) ESCAPE '\')
                  AND ($2::text IS NULL OR lower(coalesce(group_name, '')) = lower($2))
                  AND ($5::bool IS NULL OR enabled = $5)
                ORDER BY channel_number, id
                LIMIT $3 OFFSET $4
            )
            SELECT
                c.id,
                c.channel_number,
                c.name,
                coalesce(c.group_name, 'Uncategorized') AS group_name,
                c.logo_url,
                c.enabled,
                coalesce(cs.stream_count, 0) AS stream_count,
                coalesce(m.epg_mapped, false) AS epg_mapped
            FROM page_ids
            JOIN channels c ON c.id = page_ids.id
            LEFT JOIN (
                SELECT channel_id, count(*) AS stream_count
                FROM channel_streams
                WHERE channel_id IN (SELECT id FROM page_ids)
                GROUP BY channel_id
            ) cs ON cs.channel_id = c.id
            LEFT JOIN (
                SELECT channel_id, true AS epg_mapped
                FROM channel_epg_mappings
                WHERE channel_id IN (SELECT id FROM page_ids)
            ) m ON m.channel_id = c.id
            ORDER BY c.channel_number, c.id
            ",
        )
        .bind(pattern.as_deref())
        .bind(group_filter)
        .bind(limit)
        .bind(offset)
        .bind(query.enabled)
        .fetch_all(&self.pool)
        .await?;

        Ok(ChannelPage {
            total,
            limit,
            offset,
            items: rows,
        })
    }

    /// Lists distinct channel groups with total and enabled channel counts.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_groups(&self) -> Result<Vec<GroupRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, GroupRow>(
            r"
            SELECT coalesce(group_name, 'Uncategorized') AS name,
                   count(*) AS channel_count,
                   count(*) FILTER (WHERE enabled) AS enabled_count
            FROM channels
            GROUP BY group_name
            ORDER BY count(*) DESC, name
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Lists programmes for mapped canonical channels with pagination.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    #[allow(clippy::too_many_lines)]
    pub async fn list_programmes(
        &self,
        query: ProgrammeQuery,
    ) -> Result<ProgrammePage, PersistenceError> {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let offset = query.offset.unwrap_or(0).max(0);
        let now = query.now.unwrap_or_else(Utc::now);
        let pattern = query.search.as_deref().map(|value| {
            format!(
                "%{}%",
                value
                    .trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });

        let total: i64 = sqlx::query_scalar(
            r"
            WITH programme_rows AS (
                SELECT m.channel_id, c.name AS channel_name, p.title,
                       p.starts_at, coalesce(p.stops_at, 'infinity'::timestamptz) AS stops_at
                FROM programmes p
                JOIN epg_channels ec ON ec.id = p.epg_channel_id
                JOIN channel_epg_mappings m ON m.epg_channel_id = ec.id
                JOIN channels c ON c.id = m.channel_id
                WHERE c.enabled
                UNION ALL
                SELECT gp.channel_id, c.name, gp.title, gp.starts_at, gp.stops_at
                FROM generated_programmes gp
                JOIN event_channels ec ON ec.id = gp.event_channel_id
                JOIN channels c ON c.id = gp.channel_id
                WHERE c.enabled AND ec.state = 'scheduled'
            )
            SELECT count(*) FROM programme_rows
            WHERE ($1::uuid IS NULL OR channel_id = $1)
              AND ($2::timestamptz IS NULL OR starts_at >= $2)
              AND ($3::text IS NULL
                   OR lower(title) LIKE lower($3) ESCAPE '\'
                   OR lower(channel_name) LIKE lower($3) ESCAPE '\')
            ",
        )
        .bind(query.channel_id)
        .bind(query.from)
        .bind(pattern.as_deref())
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, ProgrammeRow>(
            r"
            WITH programme_rows AS (
                SELECT concat(m.channel_id::text, ':', p.id::text) AS id,
                       m.channel_id, c.name AS channel_name, p.title,
                       p.subtitle, p.description, p.categories, p.starts_at,
                       coalesce(p.stops_at, 'infinity'::timestamptz) AS stops_at,
                       es.name AS source_name
                FROM programmes p
                JOIN epg_channels ec ON ec.id = p.epg_channel_id
                JOIN channel_epg_mappings m ON m.epg_channel_id = ec.id
                JOIN channels c ON c.id = m.channel_id
                LEFT JOIN epg_sources es ON es.id = ec.epg_source_id
                WHERE c.enabled
                UNION ALL
                SELECT concat(gp.channel_id::text, ':generated:', gp.id::text),
                       gp.channel_id, c.name, gp.title, NULL::text,
                       gp.source_title, gp.categories, gp.starts_at, gp.stops_at,
                       gp.rule_name
                FROM generated_programmes gp
                JOIN event_channels ec ON ec.id = gp.event_channel_id
                JOIN channels c ON c.id = gp.channel_id
                WHERE c.enabled AND ec.state = 'scheduled'
            )
            SELECT * FROM programme_rows
            WHERE ($1::uuid IS NULL OR channel_id = $1)
              AND ($2::timestamptz IS NULL OR starts_at >= $2)
              AND ($3::text IS NULL
                   OR lower(title) LIKE lower($3) ESCAPE '\'
                   OR lower(channel_name) LIKE lower($3) ESCAPE '\')
            ORDER BY
                CASE
                    WHEN starts_at <= $4 AND stops_at > $4 THEN 0
                    WHEN starts_at > $4 THEN 1
                    ELSE 2
                END,
                CASE WHEN starts_at > $4 THEN starts_at END ASC NULLS LAST,
                starts_at DESC,
                id DESC
            LIMIT $5 OFFSET $6
            ",
        )
        .bind(query.channel_id)
        .bind(query.from)
        .bind(pattern.as_deref())
        .bind(now)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(ProgrammePage {
            total,
            limit,
            offset,
            items: rows,
        })
    }

    /// Returns real system counts for the control API overview.
    ///
    /// Uses index-only scans via partial indexes where possible.
    /// The healthy stream count uses `EXISTS` instead of `count(DISTINCT)`
    /// to avoid an expensive external merge sort on large datasets.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any count query fails.
    #[allow(clippy::cast_precision_loss, clippy::missing_errors_doc)]
    pub async fn system_counts(&self) -> Result<SystemCounts, PersistenceError> {
        let channel_count: i64 = sqlx::query_scalar("SELECT count(*) FROM channels WHERE enabled")
            .fetch_one(&self.pool)
            .await?;

        let healthy_stream_count: i64 = sqlx::query_scalar(
            r"
            SELECT count(*)
            FROM channels c
            WHERE c.enabled
              AND EXISTS (
                  SELECT 1 FROM channel_streams cs
                  WHERE cs.channel_id = c.id
              )
            ",
        )
        .fetch_one(&self.pool)
        .await?;

        let guide_mapped_count: i64 = sqlx::query_scalar(
            r"
            SELECT count(*)
            FROM channel_epg_mappings m
            JOIN channels c ON c.id = m.channel_id
            WHERE c.enabled
            ",
        )
        .fetch_one(&self.pool)
        .await?;

        let guide_coverage = if channel_count == 0 {
            0.0
        } else {
            (guide_mapped_count as f64 / channel_count as f64) * 100.0
        };

        Ok(SystemCounts {
            channels: channel_count,
            healthy_streams: healthy_stream_count,
            guide_coverage,
        })
    }
}

async fn active_supported_stream_count(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<u64, PersistenceError> {
    let count: i64 = sqlx::query_scalar(
        r"
        SELECT count(*)
        FROM provider_streams ps
        JOIN source_snapshots ss ON ss.id = ps.snapshot_id
        WHERE ss.provider_account_id = $1
          AND ss.status = 'active'
          AND ss.kind IN ('m3u', 'xtream')
          AND ps.supported
        ",
    )
    .bind(account_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(u64::try_from(count.max(0)).unwrap_or(u64::MAX))
}

async fn provider_reconciliation_progress(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<ProviderReconciliationRunProgress, PersistenceError> {
    let row = sqlx::query_as::<_, ProviderReconciliationRunProgress>(
        r"
        SELECT r.id AS run_id,
               r.status,
               r.partition_count::bigint AS partition_count,
               count(p.partition_number) FILTER (WHERE p.status = 'succeeded')::bigint
                    AS partitions_completed,
               r.total_keys,
               coalesce(sum(p.completed_keys), 0)::bigint AS completed_keys
        FROM provider_reconciliation_runs r
        LEFT JOIN provider_reconciliation_partitions p ON p.run_id = r.id
        WHERE r.id = $1
        GROUP BY r.id, r.status, r.partition_count, r.total_keys
        ",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        PersistenceError::InvalidSource("reconciliation run was not found".to_owned())
    })?;
    Ok(row)
}

async fn active_canonical_key_count(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<u64, PersistenceError> {
    let count: i64 = sqlx::query_scalar(
        r"
        SELECT count(*)
        FROM (
            SELECT COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key)
            FROM provider_streams ps
            JOIN source_snapshots ss ON ss.id = ps.snapshot_id
            WHERE ss.provider_account_id = $1
              AND ss.status = 'active'
              AND ss.kind IN ('m3u', 'xtream')
              AND ps.supported
            GROUP BY COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key)
        ) AS canonical_keys
        ",
    )
    .bind(account_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(u64::try_from(count.max(0)).unwrap_or(u64::MAX))
}

async fn canonical_key_page(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    after: Option<&str>,
    batch_size: i64,
) -> Result<Vec<String>, PersistenceError> {
    sqlx::query_scalar(
        r"
        SELECT COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) AS canonical_key
        FROM provider_streams ps
        JOIN source_snapshots ss ON ss.id = ps.snapshot_id
        WHERE ss.provider_account_id = $1
          AND ss.status = 'active'
          AND ss.kind IN ('m3u', 'xtream')
          AND ps.supported
          AND ($2::text IS NULL
               OR COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) > $2)
        GROUP BY canonical_key
        ORDER BY canonical_key
        LIMIT $3
        ",
    )
    .bind(account_id)
    .bind(after)
    .bind(batch_size)
    .fetch_all(&mut **transaction)
    .await
    .map_err(PersistenceError::from)
}

async fn reconcile_canonical_key_batches(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    batch_size: i64,
    keys_total: u64,
    active_streams: u64,
    progress: &dyn ProviderReconciliationProgress,
    phase: ProviderReconciliationPhase,
) -> Result<(i64, u64), PersistenceError> {
    let mut after = None;
    let mut keys_completed = 0_u64;
    let mut rows_affected = 0_i64;
    loop {
        let keys =
            canonical_key_page(transaction, account_id, after.as_deref(), batch_size).await?;
        if keys.is_empty() {
            break;
        }
        let affected = match phase {
            ProviderReconciliationPhase::Channels => {
                upsert_canonical_channels_batch(transaction, account_id, &keys)
                    .await?
                    .channels
            }
            ProviderReconciliationPhase::StreamLinks => {
                upsert_channel_stream_links_batch(transaction, account_id, &keys).await?
            }
            _ => {
                return Err(PersistenceError::InvalidSource(
                    "invalid reconciliation batch phase".to_owned(),
                ));
            }
        };
        rows_affected = rows_affected.saturating_add(affected);
        keys_completed =
            keys_completed.saturating_add(u64::try_from(keys.len()).unwrap_or(u64::MAX));
        after = keys.last().cloned();
        progress
            .checkpoint(ProviderReconciliationUpdate {
                phase,
                rows_affected: u64::try_from(affected.max(0)).unwrap_or(u64::MAX),
                active_streams,
                keys_completed,
                keys_total,
            })
            .await?;
    }
    Ok((rows_affected, keys_completed))
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReconcileStats {
    pub channels: i64,
    pub orphaned_channels_removed: i64,
    pub stream_links: i64,
}

/// The active stream source for a canonical channel, looked up from the
/// database for the admin preview proxy.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ChannelStreamSourceRow {
    pub channel_id: Uuid,
    pub provider_account_id: Uuid,
    pub stream_url: String,
    pub url_secret_ciphertext: Option<Vec<u8>>,
    pub channel_number: String,
}

/// One ordered provider candidate for a production channel session.
///
/// The URL template can contain redacted placeholders. The ciphertext holds
/// the complete URL and must stay inside the trusted server process.
#[derive(Clone, FromRow)]
pub struct ChannelPlaybackCandidateRow {
    pub channel_id: Uuid,
    pub channel_name: String,
    pub channel_revision: i64,
    pub provider_stream_id: Uuid,
    pub source_snapshot_id: Uuid,
    pub provider_account_id: Uuid,
    pub provider_revision: i64,
    pub provider_pool_id: Uuid,
    pub max_connections: i32,
    pub input_adapter: String,
    pub stream_url: String,
    pub url_secret_ciphertext: Option<Vec<u8>>,
    pub priority: i32,
    pub quality_rank: i32,
    pub health_status: String,
    pub bitrate_kbps: Option<i32>,
    pub alternative_base_urls: serde_json::Value,
}

impl fmt::Debug for ChannelPlaybackCandidateRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelPlaybackCandidateRow")
            .field("channel_id", &self.channel_id)
            .field("channel_name", &self.channel_name)
            .field("channel_revision", &self.channel_revision)
            .field("provider_stream_id", &self.provider_stream_id)
            .field("source_snapshot_id", &self.source_snapshot_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("provider_revision", &self.provider_revision)
            .field("provider_pool_id", &self.provider_pool_id)
            .field("max_connections", &self.max_connections)
            .field("input_adapter", &self.input_adapter)
            .field("stream_url", &"<redacted>")
            .field("url_secret_ciphertext", &"<redacted>")
            .field("priority", &self.priority)
            .field("quality_rank", &self.quality_rank)
            .field("health_status", &self.health_status)
            .field("bitrate_kbps", &self.bitrate_kbps)
            .finish()
    }
}

/// The complete ordered playback policy for one canonical channel.
#[derive(Clone, Debug)]
pub struct ChannelPlaybackPlan {
    pub channel_id: Uuid,
    pub channel_name: String,
    pub channel_revision: i64,
    pub candidates: Vec<ChannelPlaybackCandidateRow>,
}

#[allow(clippy::missing_errors_doc)]
impl CatalogRepository {
    /// Returns all active playback candidates with their effective pool caps.
    ///
    /// A connection pool overrides an account cap. Candidate order prefers a
    /// healthy stream, then the stored quality rank, then the manual priority.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn channel_playback_plan(
        &self,
        channel_id: Uuid,
    ) -> Result<Option<ChannelPlaybackPlan>, PersistenceError> {
        let candidates = sqlx::query_as::<_, ChannelPlaybackCandidateRow>(
            r"
            SELECT
                c.id AS channel_id,
                c.name AS channel_name,
                c.revision AS channel_revision,
                ps.id AS provider_stream_id,
                ss.id AS source_snapshot_id,
                ps.provider_account_id,
                pa.revision AS provider_revision,
                COALESCE(pa.connection_pool_id, pa.id) AS provider_pool_id,
                COALESCE(cp.max_connections, pa.max_connections) AS max_connections,
                pa.input_adapter,
                ps.url_template AS stream_url,
                ps.url_secret_ciphertext,
                cs.priority,
                cs.quality_rank,
                ps.health_status,
                ps.bitrate_kbps
                ,pa.alternative_base_urls
            FROM channels c
            JOIN channel_streams cs ON cs.channel_id = c.id
            JOIN provider_streams ps ON ps.id = cs.provider_stream_id
            JOIN provider_accounts pa ON pa.id = ps.provider_account_id
            LEFT JOIN connection_pools cp ON cp.id = pa.connection_pool_id
            JOIN source_snapshots ss ON ss.id = ps.snapshot_id
            WHERE c.id = $1
              AND c.enabled
              AND pa.enabled
              AND ps.supported
              AND ss.status = 'active'
            ORDER BY
                CASE ps.health_status
                    WHEN 'alive' THEN 0
                    WHEN 'unknown' THEN 1
                    WHEN 'checking' THEN 2
                    WHEN 'dead' THEN 3
                    ELSE 4
                END,
                cs.quality_rank,
                cs.priority,
                ps.id
            ",
        )
        .bind(channel_id)
        .fetch_all(&self.pool)
        .await?;

        let Some(first) = candidates.first() else {
            return Ok(None);
        };
        Ok(Some(ChannelPlaybackPlan {
            channel_id: first.channel_id,
            channel_name: first.channel_name.clone(),
            channel_revision: first.channel_revision,
            candidates,
        }))
    }

    /// Returns the highest-priority active stream URL for the given channel.
    ///
    /// The query joins `channel_streams` to `provider_streams` through the
    /// active snapshot, selecting the stream with the lowest priority value.
    pub async fn channel_stream_source(
        &self,
        channel_id: Uuid,
    ) -> Result<Option<ChannelStreamSourceRow>, PersistenceError> {
        let row = sqlx::query_as::<_, ChannelStreamSourceRow>(
            r"
            SELECT
                c.id AS channel_id,
                ps.provider_account_id,
                ps.url_template AS stream_url,
                ps.url_secret_ciphertext AS url_secret_ciphertext,
                c.channel_number
            FROM channels c
            JOIN channel_streams cs ON cs.channel_id = c.id
            JOIN provider_streams ps ON ps.id = cs.provider_stream_id
            JOIN source_snapshots ss ON ss.id = ps.snapshot_id
            WHERE c.id = $1
              AND c.enabled
              AND ss.activated_at IS NOT NULL
            ORDER BY cs.priority ASC
            LIMIT 1
            ",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Sets the `enabled` flag for a single channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn set_channel_enabled(
        &self,
        channel_id: Uuid,
        enabled: bool,
    ) -> Result<u64, PersistenceError> {
        let rows =
            sqlx::query("UPDATE channels SET enabled = $2, updated_at = now() WHERE id = $1")
                .bind(channel_id)
                .bind(enabled)
                .execute(&self.pool)
                .await?
                .rows_affected();
        Ok(rows)
    }

    /// Lists all enabled channels for playlist and XMLTV output.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_enabled_channels(&self) -> Result<Vec<ChannelRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, ChannelRow>(
            r"
            SELECT
                c.id,
                c.channel_number,
                c.name,
                coalesce(c.group_name, 'Uncategorized') AS group_name,
                c.logo_url,
                c.enabled,
                coalesce(cs.stream_count, 0) AS stream_count,
                (m.channel_id IS NOT NULL) AS epg_mapped
            FROM channels c
            LEFT JOIN (
                SELECT channel_id, count(*) AS stream_count
                FROM channel_streams
                GROUP BY channel_id
            ) cs ON cs.channel_id = c.id
            LEFT JOIN channel_epg_mappings m ON m.channel_id = c.id
            WHERE c.enabled
            ORDER BY c.channel_number, c.id
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates or seeds the fixed environment output profile.
    ///
    /// On first creation the environment token hash is seeded. On subsequent
    /// calls the persisted current token hash is retained so a rotated token
    /// stays active across restarts. Only non-token fields (`name`,
    /// `tuner_count`) update on conflict. The returned profile and all
    /// persisted diagnostics exclude token hashes.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::InvalidOutputProfileTunerCount`] when the
    /// tuner count is less than one.
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn ensure_environment_output_profile(
        &self,
        token_hash: OutputProfileTokenHash,
        tuner_count: i32,
    ) -> Result<OutputProfileRow, PersistenceError> {
        if tuner_count < 1 {
            return Err(PersistenceError::InvalidOutputProfileTunerCount);
        }
        let row = sqlx::query_as::<_, OutputProfileRow>(
            r"
            INSERT INTO output_profiles
                (id, name, token_hash, tuner_count, include_all_channels)
            VALUES ($1, $2, $3, $4, true)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                tuner_count = EXCLUDED.tuner_count,
                updated_at = CASE
                    WHEN output_profiles.tuner_count <> EXCLUDED.tuner_count
                      OR output_profiles.name <> EXCLUDED.name
                    THEN now()
                    ELSE output_profiles.updated_at
                END,
                revision = CASE
                    WHEN output_profiles.tuner_count <> EXCLUDED.tuner_count
                      OR output_profiles.name <> EXCLUDED.name
                    THEN output_profiles.revision + 1
                    ELSE output_profiles.revision
                END
            RETURNING id, name, tuner_count, include_all_channels, enabled, revision
            ",
        )
        .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .bind(ENVIRONMENT_OUTPUT_PROFILE_NAME)
        .bind(token_hash.as_bytes().as_slice())
        .bind(tuner_count)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Returns the persisted current token hash for the environment profile.
    ///
    /// Returns `None` when the environment profile does not exist. The returned
    /// hash is never serialized in API responses.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn environment_output_profile_token_hash(
        &self,
    ) -> Result<Option<OutputProfileTokenHash>, PersistenceError> {
        let row = sqlx::query_as::<_, (Option<Vec<u8>>,)>(
            r"
            SELECT token_hash FROM output_profiles WHERE id = $1
            ",
        )
        .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(hash,)| hash).and_then(|bytes| {
            bytes
                .try_into()
                .map(OutputProfileTokenHash::from_sha256)
                .ok()
        }))
    }

    /// Resolves an enabled output profile by a current or valid prior hash.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn resolve_enabled_output_profile(
        &self,
        token_hash: &OutputProfileTokenHash,
    ) -> Result<Option<OutputProfileRow>, PersistenceError> {
        let row = sqlx::query_as::<_, OutputProfileRow>(
            r"
            SELECT id, name, tuner_count, include_all_channels, enabled, revision
            FROM output_profiles
            WHERE enabled
              AND (
                  token_hash = $1
                  OR (
                      previous_token_hash = $1
                      AND previous_token_expires_at > now()
                  )
              )
            LIMIT 1
            ",
        )
        .bind(token_hash.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Rotates the environment output profile token hash.
    ///
    /// The previous token hash stays valid through the supplied overlap window
    /// in seconds. Only the hash is stored; the caller receives the plaintext
    /// token once and must not persist it.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn rotate_environment_output_profile_token(
        &self,
        new_token_hash: OutputProfileTokenHash,
        overlap_seconds: i64,
    ) -> Result<OutputProfileRow, PersistenceError> {
        let row = sqlx::query_as::<_, OutputProfileRow>(
            r"
            UPDATE output_profiles SET
                previous_token_hash = token_hash,
                previous_token_expires_at = now() + ($2::bigint * interval '1 second'),
                token_hash = $1,
                updated_at = now(),
                revision = revision + 1
            WHERE id = $3
            RETURNING id, name, tuner_count, include_all_channels, enabled, revision
            ",
        )
        .bind(new_token_hash.as_bytes().as_slice())
        .bind(overlap_seconds)
        .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Lists enabled channels that an enabled output profile publishes.
    ///
    /// A profile in all-channel mode orders channels by channel number. A
    /// subset profile orders channels by its stored position.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_enabled_channels_for_output_profile(
        &self,
        output_profile_id: Uuid,
    ) -> Result<Vec<ChannelRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, ChannelRow>(
            r"
            SELECT
                c.id,
                c.channel_number,
                coalesce(channel_alias.canonical_name, c.name) AS name,
                coalesce(c.group_name, 'Uncategorized') AS group_name,
                c.logo_url,
                c.enabled,
                coalesce(cs.stream_count, 0) AS stream_count,
                (m.channel_id IS NOT NULL) AS epg_mapped
            FROM output_profiles op
            JOIN channels c ON c.enabled
            LEFT JOIN LATERAL (
                SELECT ca.canonical_name
                FROM channel_aliases ca
                WHERE normalize_channel_name(ca.alias) = normalize_channel_name(c.name)
                   OR normalize_channel_name(ca.canonical_name) = normalize_channel_name(c.name)
                ORDER BY ca.canonical_name
                LIMIT 1
            ) channel_alias ON true
            LEFT JOIN output_profile_channels opc
                ON opc.output_profile_id = op.id AND opc.channel_id = c.id
            LEFT JOIN (
                SELECT channel_id, count(*) AS stream_count
                FROM channel_streams
                GROUP BY channel_id
            ) cs ON cs.channel_id = c.id
            LEFT JOIN channel_epg_mappings m ON m.channel_id = c.id
            WHERE op.id = $1
              AND op.enabled
              AND (op.include_all_channels OR opc.channel_id IS NOT NULL)
            ORDER BY
                CASE WHEN op.include_all_channels THEN c.channel_number END,
                CASE WHEN NOT op.include_all_channels THEN opc.position END,
                c.id
            ",
        )
        .bind(output_profile_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Returns true when an enabled output profile publishes an enabled channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn output_profile_has_channel(
        &self,
        output_profile_id: Uuid,
        channel_id: Uuid,
    ) -> Result<bool, PersistenceError> {
        let exists = sqlx::query_scalar(
            r"
            SELECT EXISTS (
                SELECT 1
                FROM output_profiles op
                JOIN channels c ON c.id = $2 AND c.enabled
                LEFT JOIN output_profile_channels opc
                    ON opc.output_profile_id = op.id AND opc.channel_id = c.id
                WHERE op.id = $1
                  AND op.enabled
                  AND (op.include_all_channels OR opc.channel_id IS NOT NULL)
            )
            ",
        )
        .bind(output_profile_id)
        .bind(channel_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Lists programmes for a set of channel IDs.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_programmes_for_channels(
        &self,
        channel_ids: &[Uuid],
    ) -> Result<Vec<ProgrammeRow>, PersistenceError> {
        if channel_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, ProgrammeRow>(
            r"
            WITH imported_programmes AS (
                SELECT
                    concat(m.channel_id::text, ':', p.id::text) AS id,
                    m.channel_id,
                    c.name AS channel_name,
                    p.title,
                    p.subtitle,
                    p.description,
                    p.categories,
                    p.starts_at,
                    coalesce(p.stops_at, 'infinity'::timestamptz) AS stops_at,
                    es.name AS source_name
                FROM programmes p
                JOIN epg_channels ec ON ec.id = p.epg_channel_id
                JOIN channel_epg_mappings m ON m.epg_channel_id = ec.id
                JOIN channels c ON c.id = m.channel_id
                LEFT JOIN epg_sources es ON es.id = ec.epg_source_id
                WHERE m.channel_id = ANY($1)
            ), dynamic_programmes AS (
                SELECT
                    concat(gp.channel_id::text, ':generated:', gp.id::text) AS id,
                    gp.channel_id,
                    c.name AS channel_name,
                    gp.title,
                    NULL::text AS subtitle,
                    gp.source_title AS description,
                    gp.categories,
                    gp.starts_at,
                    gp.stops_at,
                    gp.rule_name AS source_name
                FROM generated_programmes gp
                JOIN event_channels ec ON ec.id = gp.event_channel_id
                JOIN channels c ON c.id = gp.channel_id
                WHERE gp.channel_id = ANY($1)
                  AND ec.state = 'scheduled'
            )
            SELECT * FROM imported_programmes
            UNION ALL
            SELECT * FROM dynamic_programmes
            ORDER BY channel_id, starts_at, id
            ",
        )
        .bind(channel_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Sets the `enabled` flag for all channels in a group.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn set_group_enabled(
        &self,
        group_name: &str,
        enabled: bool,
    ) -> Result<u64, PersistenceError> {
        let rows = sqlx::query(
            "UPDATE channels SET enabled = $2, updated_at = now() WHERE group_name = $1 AND enabled != $2",
        )
        .bind(group_name)
        .bind(enabled)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows)
    }

    /// Sets the enabled flag for all channels in a single query.
    ///
    /// Only rows that differ from the target state are updated, so a no-op
    /// call (all channels already in the desired state) completes in
    /// constant time.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn set_all_groups_enabled(&self, enabled: bool) -> Result<u64, PersistenceError> {
        let rows =
            sqlx::query("UPDATE channels SET enabled = $1, updated_at = now() WHERE enabled != $1")
                .bind(enabled)
                .execute(&self.pool)
                .await?
                .rows_affected();
        Ok(rows)
    }

    /// Lists event templates, optionally restricted to enabled templates.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_event_templates(
        &self,
        query: EventTemplateQuery,
    ) -> Result<Vec<EventTemplateRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, EventTemplateRow>(
            r"
            SELECT id, name, display_name, match_regex, channel_name_format,
                   group_name, event_duration_hours, past_date_grace_hours,
                   future_date_days, timezone, filler_title, enabled
            FROM event_templates
            WHERE ($1::boolean IS FALSE OR enabled = true)
            ORDER BY name
            ",
        )
        .bind(query.enabled_only)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates a new event template.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_event_template(
        &self,
        input: CreateEventTemplate,
    ) -> Result<EventTemplateRow, PersistenceError> {
        validate_event_template_settings(&input.timezone, &input.filler_title)?;
        let row = sqlx::query_as::<_, EventTemplateRow>(
            r"
            INSERT INTO event_templates
                (name, display_name, match_regex, channel_name_format, group_name,
                 event_duration_hours, past_date_grace_hours, future_date_days,
                 timezone, filler_title)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING id, name, display_name, match_regex, channel_name_format,
                      group_name, event_duration_hours, past_date_grace_hours,
                      future_date_days, timezone, filler_title, enabled
            ",
        )
        .bind(input.name)
        .bind(input.display_name)
        .bind(input.match_regex)
        .bind(input.channel_name_format)
        .bind(input.group_name)
        .bind(input.event_duration_hours)
        .bind(input.past_date_grace_hours)
        .bind(input.future_date_days)
        .bind(input.timezone)
        .bind(input.filler_title)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Updates an existing event template.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the update fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_event_template(
        &self,
        id: Uuid,
        input: CreateEventTemplate,
    ) -> Result<EventTemplateRow, PersistenceError> {
        validate_event_template_settings(&input.timezone, &input.filler_title)?;
        let row = sqlx::query_as::<_, EventTemplateRow>(
            r"
            UPDATE event_templates SET
                name = $2,
                display_name = $3,
                match_regex = $4,
                channel_name_format = $5,
                group_name = $6,
                event_duration_hours = $7,
                past_date_grace_hours = $8,
                future_date_days = $9,
                timezone = $10,
                filler_title = $11,
                updated_at = now()
            WHERE id = $1
            RETURNING id, name, display_name, match_regex, channel_name_format,
                      group_name, event_duration_hours, past_date_grace_hours,
                      future_date_days, timezone, filler_title, enabled
            ",
        )
        .bind(id)
        .bind(input.name)
        .bind(input.display_name)
        .bind(input.match_regex)
        .bind(input.channel_name_format)
        .bind(input.group_name)
        .bind(input.event_duration_hours)
        .bind(input.past_date_grace_hours)
        .bind(input.future_date_days)
        .bind(input.timezone)
        .bind(input.filler_title)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or(PersistenceError::SourceNotFound(id))
    }

    /// Applies a partial update to an event template.
    ///
    /// Only the `Some` fields of `input` are written. `enabled` is persisted
    /// when supplied. An empty update still refreshes `updated_at`.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the update fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_event_template_partial(
        &self,
        id: Uuid,
        input: &EventTemplateUpdate,
    ) -> Result<EventTemplateRow, PersistenceError> {
        if let Some(timezone) = input.timezone.as_deref() {
            validate_event_template_timezone(timezone)?;
        }
        if let Some(filler_title) = input.filler_title.as_deref()
            && filler_title.trim().is_empty()
        {
            return Err(PersistenceError::InvalidSource(
                "filler title must not be blank".to_owned(),
            ));
        }
        let row = sqlx::query_as::<_, EventTemplateRow>(
            r"
            UPDATE event_templates SET
                name = COALESCE($2, name),
                display_name = COALESCE($3, display_name),
                match_regex = COALESCE($4, match_regex),
                channel_name_format = COALESCE($5, channel_name_format),
                group_name = COALESCE($6, group_name),
                event_duration_hours = COALESCE($7, event_duration_hours),
                past_date_grace_hours = COALESCE($8, past_date_grace_hours),
                future_date_days = COALESCE($9, future_date_days),
                timezone = COALESCE($10, timezone),
                filler_title = COALESCE($11, filler_title),
                enabled = COALESCE($12, enabled),
                updated_at = now()
            WHERE id = $1
            RETURNING id, name, display_name, match_regex, channel_name_format,
                      group_name, event_duration_hours, past_date_grace_hours,
                      future_date_days, timezone, filler_title, enabled
            ",
        )
        .bind(id)
        .bind(input.name.as_deref())
        .bind(input.display_name.as_deref())
        .bind(input.match_regex.as_deref())
        .bind(input.channel_name_format.as_deref())
        .bind(input.group_name.as_deref())
        .bind(input.event_duration_hours)
        .bind(input.past_date_grace_hours)
        .bind(input.future_date_days)
        .bind(input.timezone.as_deref())
        .bind(input.filler_title.as_deref())
        .bind(input.enabled)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or(PersistenceError::SourceNotFound(id))
    }

    /// Deletes an event template by ID.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the delete fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_event_template(&self, id: Uuid) -> Result<u64, PersistenceError> {
        let rows = sqlx::query("DELETE FROM event_templates WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows)
    }

    /// Lists event channels, optionally filtered by template ID.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_event_channels(
        &self,
        template_id: Option<Uuid>,
    ) -> Result<Vec<EventChannelRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, EventChannelRow>(
            r"
            SELECT id, template_id, channel_id, slot_number, event_title,
                   event_start, event_end, raw_stream_name, state
            FROM event_channels
            WHERE ($1::uuid IS NULL OR template_id = $1)
            ORDER BY template_id, slot_number
            ",
        )
        .bind(template_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Inserts or updates an event channel for a template and slot.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc, clippy::too_many_arguments)]
    pub async fn upsert_event_channel(
        &self,
        template_id: Uuid,
        slot: i32,
        title: Option<&str>,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        raw_name: &str,
        state: &str,
    ) -> Result<EventChannelRow, PersistenceError> {
        let row = sqlx::query_as::<_, EventChannelRow>(
            r"
            INSERT INTO event_channels
                (template_id, slot_number, event_title, event_start, event_end,
                 raw_stream_name, state)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (template_id, slot_number) DO UPDATE SET
                event_title = EXCLUDED.event_title,
                event_start = EXCLUDED.event_start,
                event_end = EXCLUDED.event_end,
                raw_stream_name = EXCLUDED.raw_stream_name,
                state = EXCLUDED.state,
                updated_at = now()
            RETURNING id, template_id, channel_id, slot_number, event_title,
                      event_start, event_end, raw_stream_name, state
            ",
        )
        .bind(template_id)
        .bind(slot)
        .bind(title)
        .bind(start)
        .bind(end)
        .bind(raw_name)
        .bind(state)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Scans provider streams and replaces only successful generated guides.
    ///
    /// A source title must match both the stored template and one built-in
    /// sports rule. A scan with no valid schedules preserves prior output.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any query fails.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn scan_event_channels(&self, template_id: Uuid) -> Result<u64, PersistenceError> {
        let template = sqlx::query_as::<_, EventTemplateRow>(
            r"
            SELECT id, name, display_name, match_regex, channel_name_format,
                   group_name, event_duration_hours, past_date_grace_hours,
                   future_date_days, timezone, filler_title, enabled
            FROM event_templates
            WHERE id = $1
            ",
        )
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(PersistenceError::SourceNotFound(template_id))?;
        if !template.enabled {
            return Ok(0);
        }

        let stream_matches = sqlx::query_as::<_, EventStreamMatch>(
            r"
            SELECT ps.name, ps.group_name, (
                SELECT COALESCE(
                    NULLIF(
                        substring(ps.name FROM '([0-9]+)\\s*(?:vs|@|at)[-[:space:]]')::int,
                        0
                    ),
                    NULLIF(
                        substring(ps.name FROM '(?:Event|GAME|MATCH)\\s*([0-9]+)')::int,
                        0
                    ),
                    NULLIF(
                        substring(ps.name FROM '([0-9]+)$')::int,
                        0
                    ),
                    1
                )
            ) AS slot
            FROM provider_streams ps
            JOIN source_snapshots ss ON ss.id = ps.snapshot_id
            WHERE ss.status = 'active'
              AND ps.supported
              AND ps.name ~ $1
            ",
        )
        .bind(&template.match_regex)
        .fetch_all(&self.pool)
        .await?;
        let mut rules = builtin_sports_event_rules();
        let duration_seconds = u64::try_from(template.event_duration_hours.max(1))
            .unwrap_or(u64::MAX)
            .saturating_mul(3_600);
        for rule in &mut rules.0 {
            rule.timezone.clone_from(&template.timezone);
            rule.guide.duration_seconds = duration_seconds;
            rule.guide.filler_title.clone_from(&template.filler_title);
            rule.guide
                .title_template
                .clone_from(&template.channel_name_format);
        }
        let compiled = CompiledEventRules::new(rules)
            .map_err(|error| PersistenceError::InvalidSource(error.to_string()))?;
        let metadata = BTreeMap::new();
        let mut matched_by_slot = BTreeMap::new();
        for stream in stream_matches {
            let candidate = EventCandidate {
                name: &stream.name,
                group: stream.group_name.as_deref(),
                epg_title: None,
                categories: &[],
                metadata: &metadata,
            };
            let Ok(Some(event_match)) = compiled.first_match(&candidate) else {
                continue;
            };
            matched_by_slot
                .entry(stream.slot.max(1))
                .or_insert_with(Vec::new)
                .push((stream.name, event_match));
        }
        let mut updated = 0_u64;
        for (slot, matched) in matched_by_slot {
            let inputs = matched
                .iter()
                .map(|(source_title, matched)| EventGuideInput {
                    matched,
                    source_title,
                })
                .collect::<Vec<_>>();
            let Some(first_start) = matched.iter().filter_map(|(_, item)| item.starts_at).min()
            else {
                continue;
            };
            let Some(last_start) = matched.iter().filter_map(|(_, item)| item.starts_at).max()
            else {
                continue;
            };
            let window_start = first_start
                .checked_sub_signed(Duration::hours(i64::from(
                    template.past_date_grace_hours.max(0),
                )))
                .unwrap_or(first_start);
            let window_end = last_start
                .checked_add_signed(Duration::hours(i64::from(
                    template.event_duration_hours.max(1),
                )))
                .and_then(|time| {
                    time.checked_add_signed(Duration::days(i64::from(
                        template.future_date_days.max(0),
                    )))
                })
                .unwrap_or(last_start);
            let Ok(schedule) =
                generate_event_schedule(&inputs, window_start, window_end, &template.filler_title)
            else {
                continue;
            };
            let Some(event) = schedule
                .iter()
                .find(|programme| programme.kind == GeneratedProgrammeKind::Event)
            else {
                continue;
            };
            let channel_id: Option<Uuid> = sqlx::query_scalar(
                r"
                SELECT id FROM channels
                WHERE coalesce(group_name, 'Uncategorized') = $1 AND enabled
                ORDER BY channel_number
                LIMIT 1
                ",
            )
            .bind(&template.group_name)
            .fetch_optional(&self.pool)
            .await?;
            let Some(channel_id) = channel_id else {
                continue;
            };
            let existing_event_channel_id: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM event_channels WHERE template_id = $1 AND slot_number = $2",
            )
            .bind(template.id)
            .bind(slot)
            .fetch_optional(&self.pool)
            .await?;
            let event_channel_id = if let Some(id) = existing_event_channel_id {
                id
            } else {
                self.upsert_event_channel(
                    template.id,
                    slot,
                    Some(&event.title),
                    Some(event.starts_at),
                    Some(event.stops_at),
                    &event.source_title,
                    "scheduled",
                )
                .await?
                .id
            };
            if !self
                .replace_generated_programmes(event_channel_id, channel_id, &template, &schedule)
                .await?
            {
                continue;
            }
            self.upsert_event_channel(
                template.id,
                slot,
                Some(&event.title),
                Some(event.starts_at),
                Some(event.stops_at),
                &event.source_title,
                "scheduled",
            )
            .await?;
            sqlx::query(
                "UPDATE event_channels SET channel_id = $2, updated_at = now() WHERE id = $1",
            )
            .bind(event_channel_id)
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
            updated = updated.saturating_add(1);
        }
        Ok(updated)
    }

    /// Scans every enabled dynamic-event template after provider reconciliation.
    #[allow(clippy::missing_errors_doc)]
    pub async fn scan_all_event_channels(&self) -> Result<u64, PersistenceError> {
        let templates = self
            .list_event_templates(EventTemplateQuery { enabled_only: true })
            .await?;
        let mut updated = 0_u64;
        for template in templates {
            updated = updated.saturating_add(self.scan_event_channels(template.id).await?);
        }
        Ok(updated)
    }

    async fn replace_generated_programmes(
        &self,
        event_channel_id: Uuid,
        channel_id: Uuid,
        template: &EventTemplateRow,
        schedule: &[GeneratedProgramme],
    ) -> Result<bool, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let starts_at = schedule
            .iter()
            .map(|programme| programme.starts_at)
            .collect::<Vec<_>>();
        let stops_at = schedule
            .iter()
            .map(|programme| programme.stops_at)
            .collect::<Vec<_>>();
        let overlaps: bool = sqlx::query_scalar(
            r"
            SELECT EXISTS (
                SELECT 1
                FROM generated_programmes existing
                CROSS JOIN unnest($3::timestamptz[], $4::timestamptz[])
                    AS incoming(starts_at, stops_at)
                WHERE existing.channel_id = $1
                  AND existing.event_channel_id <> $2
                  AND existing.starts_at < incoming.stops_at
                  AND incoming.starts_at < existing.stops_at
            )
            ",
        )
        .bind(channel_id)
        .bind(event_channel_id)
        .bind(&starts_at)
        .bind(&stops_at)
        .fetch_one(&mut *transaction)
        .await?;
        if overlaps {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM generated_programmes WHERE event_channel_id = $1")
            .bind(event_channel_id)
            .execute(&mut *transaction)
            .await?;
        for programme in schedule {
            let kind = match programme.kind {
                GeneratedProgrammeKind::Event => "event",
                GeneratedProgrammeKind::Filler => "filler",
            };
            let rule_name = if programme.kind == GeneratedProgrammeKind::Filler {
                format!("{} filler", template.display_name)
            } else {
                template.display_name.clone()
            };
            sqlx::query(
                r"
                INSERT INTO generated_programmes
                    (id, event_channel_id, channel_id, template_id, rule_id, rule_name,
                     stable_event_key, source_title, kind, starts_at, stops_at, title,
                     categories, metadata)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                ",
            )
            .bind(Uuid::now_v7())
            .bind(event_channel_id)
            .bind(channel_id)
            .bind(template.id)
            .bind(template.id)
            .bind(rule_name)
            .bind(&programme.stable_event_key)
            .bind(&programme.source_title)
            .bind(kind)
            .bind(programme.starts_at)
            .bind(programme.stops_at)
            .bind(&programme.title)
            .bind(json!(["Dynamic event", kind]))
            .bind(json!({"template": template.name, "sourceTitle": programme.source_title}))
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    /// Marks event channels as hidden when their end time plus grace has passed.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn prune_past_event_channels(
        &self,
        grace_hours: i32,
    ) -> Result<u64, PersistenceError> {
        let rows = sqlx::query(
            r"
            UPDATE event_channels SET state = 'hidden', updated_at = now()
            WHERE state <> 'hidden'
              AND event_end IS NOT NULL
              AND event_end + make_interval(hours => $1::int) < now()
            ",
        )
        .bind(grace_hours)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows)
    }

    /// Lists all lineup templates ordered by name.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_lineup_templates(&self) -> Result<Vec<LineupTemplateRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, LineupTemplateRow>(
            r"
            SELECT id, name, package_name, country, description, enabled
            FROM lineup_templates
            ORDER BY name
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates a new lineup template and returns the stored row.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert fails or the
    /// template name is not unique.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_lineup_template(
        &self,
        name: &str,
        package_name: &str,
        country: &str,
        description: Option<&str>,
    ) -> Result<LineupTemplateRow, PersistenceError> {
        let row = sqlx::query_as::<_, LineupTemplateRow>(
            r"
            INSERT INTO lineup_templates (name, package_name, country, description)
            VALUES ($1, $2, $3, $4)
            RETURNING id, name, package_name, country, description, enabled
            ",
        )
        .bind(name)
        .bind(package_name)
        .bind(country)
        .bind(description)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Deletes a lineup template by ID. Returns the number of rows removed.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_lineup_template(&self, id: Uuid) -> Result<u64, PersistenceError> {
        let rows = sqlx::query("DELETE FROM lineup_templates WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows)
    }

    /// Lists lineup categories for a template ordered by sort order.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_lineup_categories(
        &self,
        template_id: Uuid,
    ) -> Result<Vec<LineupCategoryRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, LineupCategoryRow>(
            r"
            SELECT id, template_id, name, sort_order
            FROM lineup_categories
            WHERE template_id = $1
            ORDER BY sort_order, name
            ",
        )
        .bind(template_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Lists all lineup channels for a category ordered by channel number.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_lineup_channels(
        &self,
        category_id: Uuid,
    ) -> Result<Vec<LineupChannelRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, LineupChannelRow>(
            r"
            SELECT id, category_id, name, channel_number, aliases, enabled
            FROM lineup_channels
            WHERE category_id = $1
            ORDER BY channel_number
            ",
        )
        .bind(category_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Imports categories and channels for a lineup template.
    ///
    /// Each category tuple contains a name, a sort order, and a list of
    /// channel tuples. Each channel tuple contains a name, a channel number,
    /// and a list of alias names. The method deletes existing categories and
    /// channels for the template, then inserts the new data. Returns the
    /// count of channels imported.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any insert fails.
    #[allow(clippy::missing_errors_doc, clippy::type_complexity)]
    pub async fn import_lineup(
        &self,
        template_id: Uuid,
        categories: Vec<(String, i32, Vec<(String, String, Vec<String>)>)>,
    ) -> Result<u64, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM lineup_categories WHERE template_id = $1")
            .bind(template_id)
            .execute(&mut *transaction)
            .await?;
        let mut channel_count: u64 = 0;
        for (category_name, sort_order, channels) in categories {
            let category_id: Uuid = sqlx::query_scalar(
                r"
                INSERT INTO lineup_categories (template_id, name, sort_order)
                VALUES ($1, $2, $3)
                RETURNING id
                ",
            )
            .bind(template_id)
            .bind(&category_name)
            .bind(sort_order)
            .fetch_one(&mut *transaction)
            .await?;
            for (channel_name, channel_number, aliases) in channels {
                sqlx::query(
                    r"
                    INSERT INTO lineup_channels
                        (category_id, name, channel_number, aliases)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (category_id, channel_number) DO UPDATE SET
                        name = EXCLUDED.name,
                        aliases = EXCLUDED.aliases,
                        enabled = true
                    ",
                )
                .bind(category_id)
                .bind(&channel_name)
                .bind(&channel_number)
                .bind(&aliases)
                .execute(&mut *transaction)
                .await?;
                channel_count += 1;
            }
        }
        transaction.commit().await?;
        Ok(channel_count)
    }

    /// Applies a lineup template to the canonical channel catalog.
    ///
    /// For each category, the method assigns matched channels to the category
    /// group name. For each channel in the category, it finds matching
    /// canonical channels by normalized name or alias. Matched channels are
    /// enabled and assigned to the category group. Channels in the group that
    /// do not match any lineup entry are disabled.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn apply_lineup(
        &self,
        template_id: Uuid,
    ) -> Result<LineupApplyStats, PersistenceError> {
        let categories = self.list_lineup_categories(template_id).await?;
        let mut stats = LineupApplyStats::default();
        let mut transaction = self.pool.begin().await?;
        for category in categories {
            let channels = sqlx::query_as::<_, LineupChannelRow>(
                r"
                SELECT id, category_id, name, channel_number, aliases, enabled
                FROM lineup_channels
                WHERE category_id = $1 AND enabled
                ORDER BY channel_number
                ",
            )
            .bind(category.id)
            .fetch_all(&mut *transaction)
            .await?;
            let mut matched_ids: Vec<Uuid> = Vec::new();
            // Batch the per-channel name and alias lookups into a single
            // query so a category with N lineup channels issues one round
            // trip instead of N. The result is the union of channels whose
            // normalized name matches any lineup name or alias token.
            let mut tokens: Vec<String> = Vec::with_capacity(channels.len() * 2);
            for lineup_channel in &channels {
                tokens.push(lineup_channel.name.clone());
                tokens.extend(lineup_channel.aliases.iter().cloned());
            }
            if !tokens.is_empty() {
                let matched: Vec<Uuid> = sqlx::query_scalar(
                    r"
                    SELECT c.id
                    FROM channels c
                    WHERE normalize_channel_name(c.name) IN (
                        SELECT normalize_channel_name(token)
                        FROM unnest($1::text[]) AS token
                    )
                    ",
                )
                .bind(&tokens)
                .fetch_all(&mut *transaction)
                .await?;
                matched_ids = matched;
            }
            let matched_count = u64::try_from(matched_ids.len()).unwrap_or(u64::MAX);
            stats.matched += matched_count;
            let total_lineup = u64::try_from(channels.len()).unwrap_or(u64::MAX);
            stats.unmatched += total_lineup.saturating_sub(matched_count);
            if !matched_ids.is_empty() {
                let enabled_rows = sqlx::query(
                    r"
                    UPDATE channels
                    SET enabled = true,
                        group_name = $2,
                        updated_at = now()
                    WHERE id = ANY($1)
                    ",
                )
                .bind(&matched_ids)
                .bind(&category.name)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
                stats.enabled += enabled_rows;
                let disabled_rows = sqlx::query(
                    r"
                    UPDATE channels
                    SET enabled = false, updated_at = now()
                    WHERE group_name = $1
                      AND id <> ALL($2)
                    ",
                )
                .bind(&category.name)
                .bind(&matched_ids)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
                stats.disabled += disabled_rows;
            }
        }
        transaction.commit().await?;
        Ok(stats)
    }

    /// Lists all EPG mappings with channel and EPG channel details.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_epg_mappings(
        &self,
        review_status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<EpgMappingPage, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let offset = offset.max(0);
        let total: i64 = if let Some(status) = review_status {
            sqlx::query_scalar(
                "SELECT count(*) FROM channel_epg_mappings m JOIN channels c ON c.id = m.channel_id WHERE c.enabled = true AND m.review_status = $1",
            )
            .bind(status)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query_scalar("SELECT count(*) FROM channel_epg_mappings m JOIN channels c ON c.id = m.channel_id WHERE c.enabled = true")
                .fetch_one(&self.pool)
                .await?
        };
        let rows: Vec<EpgMappingRow> = if let Some(status) = review_status {
            sqlx::query_as(
                r"
                SELECT m.channel_id, m.epg_channel_id, m.method, m.confidence,
                       m.evidence, m.review_status, m.reviewed_by, m.reviewed_at,
                       m.revision, m.updated_at,
                       c.name AS channel_name, c.canonical_key,
                       ec.xmltv_id AS epg_xmltv_id,
                       ec.display_names->0->>'value' AS epg_display_name
                FROM channel_epg_mappings m
                JOIN channels c ON c.id = m.channel_id
                LEFT JOIN epg_channels ec ON ec.id = m.epg_channel_id
                WHERE c.enabled = true AND m.review_status = $1
                ORDER BY m.confidence DESC, c.name
                LIMIT $2 OFFSET $3
                ",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                r"
                SELECT m.channel_id, m.epg_channel_id, m.method, m.confidence,
                       m.evidence, m.review_status, m.reviewed_by, m.reviewed_at,
                       m.revision, m.updated_at,
                       c.name AS channel_name, c.canonical_key,
                       ec.xmltv_id AS epg_xmltv_id,
                       ec.display_names->0->>'value' AS epg_display_name
                FROM channel_epg_mappings m
                JOIN channels c ON c.id = m.channel_id
                LEFT JOIN epg_channels ec ON ec.id = m.epg_channel_id
                WHERE c.enabled = true
                ORDER BY m.confidence DESC, c.name
                LIMIT $1 OFFSET $2
                ",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(EpgMappingPage { total, items: rows })
    }

    /// Lists channels that have no EPG mapping.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_unmapped_channels(
        &self,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<UnmappedChannelPage, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let offset = offset.max(0);
        let search_pattern = format!("%{}%", search.unwrap_or("").to_lowercase());
        let total: i64 = sqlx::query_scalar(
            r"
            SELECT count(*) FROM channels c
            WHERE c.managed_by = 'automatic'
              AND c.enabled = true
              AND c.canonical_key IS NOT NULL
              AND c.canonical_key NOT LIKE 'stream:%'
              AND NOT EXISTS (SELECT 1 FROM channel_epg_mappings m WHERE m.channel_id = c.id)
              AND ($1::text = '' OR lower(c.name) LIKE $1)
            ",
        )
        .bind(&search_pattern)
        .fetch_one(&self.pool)
        .await?;
        let rows: Vec<UnmappedChannelRow> = sqlx::query_as(
            r"
            SELECT c.id, c.name, c.canonical_key, c.group_name
            FROM channels c
            WHERE c.managed_by = 'automatic'
              AND c.enabled = true
              AND c.canonical_key IS NOT NULL
              AND c.canonical_key NOT LIKE 'stream:%'
              AND NOT EXISTS (SELECT 1 FROM channel_epg_mappings m WHERE m.channel_id = c.id)
              AND ($1::text = '' OR lower(c.name) LIKE $1)
            ORDER BY c.name
            LIMIT $2 OFFSET $3
            ",
        )
        .bind(&search_pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(UnmappedChannelPage { total, items: rows })
    }

    /// Lists review candidates for a channel that needs operator review.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_review_candidates(
        &self,
        channel_id: Uuid,
    ) -> Result<Vec<ReviewCandidateRow>, PersistenceError> {
        let rows: Vec<ReviewCandidateRow> = sqlx::query_as(
            r"
            SELECT rc.id, rc.channel_id, rc.epg_channel_id, rc.method,
                   rc.confidence, rc.evidence, rc.created_at,
                   ec.xmltv_id AS epg_xmltv_id,
                   ec.display_names->0->>'value' AS epg_display_name
            FROM review_candidates rc
            LEFT JOIN epg_channels ec ON ec.id = rc.epg_channel_id
            WHERE rc.channel_id = $1
            ORDER BY rc.confidence DESC, rc.epg_channel_id ASC
            ",
        )
        .bind(channel_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Sets a manual EPG mapping for a channel, overriding any automatic match.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn set_manual_epg_mapping(
        &self,
        channel_id: Uuid,
        epg_channel_id: Uuid,
        actor: &str,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r"
            INSERT INTO channel_epg_mappings
                (channel_id, epg_channel_id, method, confidence, evidence,
                 review_status, reviewed_by, reviewed_at, revision)
            VALUES ($1, $2, 'manual', 1.0,
                    jsonb_build_object('match', 'manual-override', 'set_by', $3),
                    'manual', $3, now(), 1)
            ON CONFLICT (channel_id) DO UPDATE SET
                epg_channel_id = EXCLUDED.epg_channel_id,
                method = EXCLUDED.method,
                confidence = EXCLUDED.confidence,
                evidence = EXCLUDED.evidence,
                review_status = EXCLUDED.review_status,
                reviewed_by = EXCLUDED.reviewed_by,
                reviewed_at = EXCLUDED.reviewed_at,
                revision = channel_epg_mappings.revision + 1,
                updated_at = now()
            ",
        )
        .bind(channel_id)
        .bind(epg_channel_id)
        .bind(actor)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes the EPG mapping for a channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn remove_epg_mapping(&self, channel_id: Uuid) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM channel_epg_mappings WHERE channel_id = $1")
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resolves a review by accepting or rejecting the candidate mapping.
    ///
    /// When accepted, updates the mapping to the selected EPG channel with
    /// `review_status = 'applied'`. When rejected, sets `review_status = 'rejected'`.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn resolve_review(
        &self,
        channel_id: Uuid,
        accept: bool,
        epg_channel_id: Option<Uuid>,
        actor: &str,
    ) -> Result<(), PersistenceError> {
        if accept {
            let epg_id = epg_channel_id.ok_or_else(|| {
                PersistenceError::Database(sqlx::Error::Configuration(
                    "epg_channel_id is required when accepting a review".into(),
                ))
            })?;
            sqlx::query(
                r"
                UPDATE channel_epg_mappings
                SET epg_channel_id = $2,
                    method = 'manual',
                    confidence = 1.0,
                    review_status = 'applied',
                    reviewed_by = $3,
                    reviewed_at = now(),
                    revision = revision + 1,
                    updated_at = now()
                WHERE channel_id = $1
                ",
            )
            .bind(channel_id)
            .bind(epg_id)
            .bind(actor)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                r"
                UPDATE channel_epg_mappings
                SET review_status = 'rejected',
                    reviewed_by = $2,
                    reviewed_at = now(),
                    revision = revision + 1,
                    updated_at = now()
                WHERE channel_id = $1
                ",
            )
            .bind(channel_id)
            .bind(actor)
            .execute(&self.pool)
            .await?;
        }
        sqlx::query("DELETE FROM review_candidates WHERE channel_id = $1")
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Searches EPG channels by name or XMLTV ID for manual mapping selection.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn search_epg_channels(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<EpgChannelSearchRow>, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let pattern = format!("%{}%", query.to_lowercase());
        let rows: Vec<EpgChannelSearchRow> = sqlx::query_as(
            r"
            SELECT ec.id, ec.xmltv_id,
                   ec.display_names->0->>'value' AS display_name
            FROM epg_channels ec
            JOIN source_snapshots ss ON ss.id = ec.source_snapshot_id AND ss.status = 'active'
            WHERE lower(ec.xmltv_id) LIKE $1
               OR lower(ec.display_names->0->>'value') LIKE $1
            ORDER BY ec.xmltv_id
            LIMIT $2
            ",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EpgMappingStats {
    pub mappings_applied: i64,
    pub mappings_removed: i64,
    pub review_queued: i64,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct EpgMappingRow {
    pub channel_id: Uuid,
    pub epg_channel_id: Uuid,
    pub method: String,
    pub confidence: f32,
    pub evidence: Value,
    pub review_status: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
    pub channel_name: String,
    pub canonical_key: Option<String>,
    pub epg_xmltv_id: Option<String>,
    pub epg_display_name: Option<String>,
}

#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct EpgMappingPage {
    pub total: i64,
    pub items: Vec<EpgMappingRow>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct UnmappedChannelRow {
    pub id: Uuid,
    pub name: String,
    pub canonical_key: Option<String>,
    pub group_name: Option<String>,
}

#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct UnmappedChannelPage {
    pub total: i64,
    pub items: Vec<UnmappedChannelRow>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ReviewCandidateRow {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub epg_channel_id: Uuid,
    pub method: String,
    pub confidence: f32,
    pub evidence: Value,
    pub created_at: DateTime<Utc>,
    pub epg_xmltv_id: Option<String>,
    pub epg_display_name: Option<String>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct EpgChannelSearchRow {
    pub id: Uuid,
    pub xmltv_id: String,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ChannelQuery {
    pub search: Option<String>,
    pub group: Option<String>,
    pub enabled: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct EventTemplateRow {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub match_regex: String,
    pub channel_name_format: String,
    pub group_name: String,
    pub event_duration_hours: i32,
    pub past_date_grace_hours: i32,
    pub future_date_days: i32,
    pub timezone: String,
    pub filler_title: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct EventChannelRow {
    pub id: Uuid,
    pub template_id: Uuid,
    pub channel_id: Option<Uuid>,
    pub slot_number: i32,
    pub event_title: Option<String>,
    pub event_start: Option<DateTime<Utc>>,
    pub event_end: Option<DateTime<Utc>>,
    pub raw_stream_name: Option<String>,
    pub state: String,
}

#[derive(Clone, Debug, Default)]
pub struct EventTemplateQuery {
    pub enabled_only: bool,
}

#[derive(Clone, Debug)]
pub struct CreateEventTemplate {
    pub name: String,
    pub display_name: String,
    pub match_regex: String,
    pub channel_name_format: String,
    pub group_name: String,
    pub event_duration_hours: i32,
    pub past_date_grace_hours: i32,
    pub future_date_days: i32,
    pub timezone: String,
    pub filler_title: String,
}

impl Default for CreateEventTemplate {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: String::new(),
            match_regex: String::new(),
            channel_name_format: String::new(),
            group_name: String::new(),
            event_duration_hours: 0,
            past_date_grace_hours: 0,
            future_date_days: 0,
            timezone: "UTC".to_owned(),
            filler_title: "No programs available".to_owned(),
        }
    }
}

/// Partial update for an event template. Only `Some` fields are applied.
/// An empty update leaves the row unchanged and still refreshes `updated_at`.
#[derive(Clone, Debug, Default)]
pub struct EventTemplateUpdate {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub match_regex: Option<String>,
    pub channel_name_format: Option<String>,
    pub group_name: Option<String>,
    pub event_duration_hours: Option<i32>,
    pub past_date_grace_hours: Option<i32>,
    pub future_date_days: Option<i32>,
    pub timezone: Option<String>,
    pub filler_title: Option<String>,
    pub enabled: Option<bool>,
}

fn validate_event_template_settings(
    timezone: &str,
    filler_title: &str,
) -> Result<(), PersistenceError> {
    validate_event_template_timezone(timezone)?;
    if filler_title.trim().is_empty() {
        return Err(PersistenceError::InvalidSource(
            "filler title must not be blank".to_owned(),
        ));
    }
    Ok(())
}

fn validate_event_template_timezone(value: &str) -> Result<(), PersistenceError> {
    let timezone = value.trim();
    if timezone != value || timezone.is_empty() {
        return Err(PersistenceError::InvalidSource(
            "event template timezone must be a nonblank IANA timezone".to_owned(),
        ));
    }
    if timezone != "UTC" && (!timezone.contains('/') || timezone.parse::<Tz>().is_err()) {
        return Err(PersistenceError::InvalidSource(
            "event template timezone must be UTC or a valid IANA timezone".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct ProgrammeQuery {
    pub channel_id: Option<Uuid>,
    pub from: Option<DateTime<Utc>>,
    pub search: Option<String>,
    /// Reference instant for deterministic current-then-upcoming ordering.
    pub now: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ChannelRow {
    pub id: Uuid,
    pub channel_number: String,
    pub name: String,
    pub group_name: String,
    pub logo_url: Option<String>,
    pub enabled: bool,
    pub stream_count: i64,
    pub epg_mapped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelPage {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub items: Vec<ChannelRow>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ProgrammeRow {
    pub id: String,
    pub channel_id: Option<Uuid>,
    pub channel_name: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Value,
    pub starts_at: DateTime<Utc>,
    pub stops_at: DateTime<Utc>,
    pub source_name: Option<String>,
}

impl ProgrammeRow {
    pub fn category_list(&self) -> Vec<String> {
        self.categories
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgrammePage {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub items: Vec<ProgrammeRow>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SystemCounts {
    pub channels: i64,
    pub healthy_streams: i64,
    pub guide_coverage: f64,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct GroupRow {
    pub name: String,
    pub channel_count: i64,
    pub enabled_count: i64,
}

// Lineup template row types for the Lineuparr-style lineup system.
#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct LineupTemplateRow {
    pub id: Uuid,
    pub name: String,
    pub package_name: String,
    pub country: String,
    pub description: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct LineupCategoryRow {
    pub id: Uuid,
    pub template_id: Uuid,
    pub name: String,
    pub sort_order: i32,
}

#[derive(Clone, Debug, Default, FromRow, Serialize)]
pub struct LineupChannelRow {
    pub id: Uuid,
    pub category_id: Uuid,
    pub name: String,
    pub channel_number: String,
    pub aliases: Vec<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LineupApplyStats {
    pub matched: u64,
    pub unmatched: u64,
    pub enabled: u64,
    pub disabled: u64,
}

#[derive(Debug, FromRow)]
struct ReconcileCountRow {
    channels: i64,
}

#[derive(Debug, FromRow)]
struct EventStreamMatch {
    name: String,
    group_name: Option<String>,
    slot: i32,
}

#[derive(Debug, FromRow)]
struct EventSuggestionStream {
    name: String,
    group_name: Option<String>,
}

/// A suggested event template configuration derived from provider stream data.
#[derive(Clone, Debug, Serialize)]
pub struct EventTemplateSuggestion {
    pub name: String,
    pub display_name: String,
    pub match_regex: String,
    pub channel_name_format: String,
    pub group_name: String,
    pub event_duration_hours: i32,
    pub past_date_grace_hours: i32,
    pub future_date_days: i32,
    pub sample_streams: Vec<String>,
    pub stream_count: u64,
}

/// Detects a grouping key from a stream name and optional group name.
///
/// Looks for known league prefixes (NBA, NHL, NFL, MLB, MLS, EPL, UFC, etc.)
/// in the stream name. Falls back to the provider group name, or "Sports"
/// if no league prefix is found.
const EVENT_LEAGUES: &[&str] = &[
    "NBA",
    "NHL",
    "NFL",
    "MLB",
    "MLS",
    "EPL",
    "UFC",
    "BOXING",
    "F1",
    "NASCAR",
    "PGA",
    "ATP",
    "WTA",
    "UCL",
    "UEL",
    "SERIE A",
    "LA LIGA",
    "BUNDESLIGA",
    "LIGUE 1",
    "CHAMPIONS",
    "EUROPA",
    "WORLD CUP",
    "COLLEGE",
    "NCAA",
    "CFL",
    "AFL",
    "RUGBY",
    "CRICKET",
    "IPL",
];

fn detect_event_group_key(stream_name: &str, group_name: Option<&str>) -> String {
    let upper = stream_name.to_uppercase();
    for league in EVENT_LEAGUES {
        if upper.contains(league) {
            return (*league).to_owned();
        }
    }
    // Fall back to the provider group name if it looks like a sports group.
    if let Some(group) = group_name {
        let group_upper = group.to_uppercase();
        if group_upper.contains("SPORT")
            || group_upper.contains("LIVE")
            || group_upper.contains("EVENT")
            || group_upper.contains("PPV")
        {
            return group.to_owned();
        }
    }
    "Sports".to_owned()
}

/// Builds a suggested template configuration from a group key and sample streams.
fn build_suggestion_config(
    group_key: &str,
    samples: &[String],
) -> (String, String, String, String) {
    let name = group_key
        .to_lowercase()
        .replace([' ', '/'], "-")
        .replace("--", "-");
    let display_name = group_key.to_owned();
    // Build a regex that matches the league prefix plus vs/@ pattern.
    let match_regex = if group_key == "Sports" {
        r"(?i).*\b(?:vs\.?|versus|@|at)\b.*".to_owned()
    } else {
        format!(r"(?i)\b{group_key}\b.*\b(?:vs\.?|versus|@|at)\b.*")
    };
    let group_name = "Sports".to_owned();
    let _ = samples;
    (name, display_name, match_regex, group_name)
}

#[allow(clippy::too_many_lines)]
async fn upsert_canonical_channels_batch(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    canonical_keys: &[String],
) -> Result<ReconcileCountRow, PersistenceError> {
    let inserted: i64 = sqlx::query(
        r"
        WITH active_snapshots AS (
            SELECT id
            FROM source_snapshots
            WHERE provider_account_id = $1
              AND kind IN ('m3u', 'xtream')
              AND status = 'active'
        ),
        grouped AS (
            SELECT
                COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) AS canonical_key,
                (array_agg(ps.name ORDER BY ps.id))[1] AS name,
                (array_agg(ps.group_name ORDER BY ps.id)
                    FILTER (WHERE ps.group_name IS NOT NULL))[1] AS group_name,
                (array_agg(ps.logo_url ORDER BY ps.id)
                    FILTER (WHERE ps.logo_url IS NOT NULL AND ps.logo_url <> ''))[1] AS logo_url,
                (array_agg(ps.channel_number ORDER BY ps.id)
                    FILTER (WHERE ps.channel_number IS NOT NULL AND ps.channel_number <> ''))[1] AS preferred_number
            FROM provider_streams ps
            WHERE ps.snapshot_id IN (SELECT id FROM active_snapshots)
              AND ps.supported
              AND COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) = ANY($2)
            GROUP BY canonical_key
        ),
        preferred_deduped AS (
            SELECT DISTINCT ON (preferred_number)
                canonical_key, name, group_name, logo_url, preferred_number
            FROM grouped
            WHERE preferred_number IS NOT NULL
            ORDER BY preferred_number, canonical_key
        ),
        all_groups AS (
            SELECT canonical_key, name, group_name, logo_url, preferred_number
            FROM preferred_deduped
            UNION ALL
            SELECT canonical_key, name, group_name, logo_url, NULL::text AS preferred_number
            FROM grouped
            WHERE preferred_number IS NULL
        ),
        channel_rows AS (
            SELECT
                COALESCE(
                    c.id,
                    regexp_replace(
                        md5('iptv-channel:v1:' || $1::text || ':' || all_groups.canonical_key),
                        '^(.{8})(.{4})(.{4})(.{4})(.{12})$',
                        '\1-\2-\3-\4-\5'
                    )::uuid
                ) AS id,
                all_groups.canonical_key,
                all_groups.name,
                all_groups.group_name,
                all_groups.logo_url,
                CASE
                    WHEN c.id IS NOT NULL THEN c.enabled
                    WHEN all_groups.group_name IS NOT NULL
                         AND EXISTS (
                             SELECT 1
                             FROM channels grouped_channel
                             WHERE grouped_channel.group_name = all_groups.group_name
                               AND grouped_channel.enabled
                         )
                    THEN true
                    WHEN all_groups.group_name IS NOT NULL
                         AND EXISTS (
                             SELECT 1
                             FROM channels grouped_channel
                             WHERE grouped_channel.group_name = all_groups.group_name
                         )
                    THEN false
                    ELSE true
                END AS effective_enabled,
                CASE
                    WHEN all_groups.preferred_number IS NOT NULL
                         AND NOT EXISTS (
                             SELECT 1 FROM channels c2
                             WHERE c2.channel_number = all_groups.preferred_number
                         )
                    THEN all_groups.preferred_number
                    ELSE nextval('canonical_channel_number_seq')::text
                END AS channel_number
            FROM all_groups
            LEFT JOIN channels c
              ON c.provider_account_id = $1
             AND c.canonical_key = all_groups.canonical_key
        )
        INSERT INTO channels
            (id, channel_number, name, group_name, logo_url, enabled,
             managed_by, provider_account_id, canonical_key, revision)
        SELECT id, channel_number, name, group_name, logo_url, effective_enabled,
               'automatic', $1, canonical_key, 1
        FROM channel_rows
        ON CONFLICT (id) DO UPDATE SET
            name = EXCLUDED.name,
            group_name = EXCLUDED.group_name,
            logo_url = EXCLUDED.logo_url,
            updated_at = now(),
            revision = channels.revision + 1
        WHERE (channels.name, channels.group_name, channels.logo_url)
              IS DISTINCT FROM
              (EXCLUDED.name, EXCLUDED.group_name, EXCLUDED.logo_url)
        ",
    )
    .bind(account_id)
    .bind(canonical_keys)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);

    Ok(ReconcileCountRow { channels: inserted })
}

async fn upsert_candidate_channels(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run_id: Uuid,
) -> Result<i64, PersistenceError> {
    let inserted: i64 = sqlx::query(
        r"
        WITH ranked_candidates AS (
            SELECT rc.*,
                   row_number() OVER (
                       PARTITION BY rc.preferred_number
                       ORDER BY rc.canonical_key
                   ) AS preferred_rank
            FROM provider_reconciliation_candidates rc
            WHERE rc.run_id = $2
        ),
        channel_rows AS (
            SELECT
                COALESCE(
                    c.id,
                    regexp_replace(
                        md5('iptv-channel:v1:' || $1::text || ':' || rc.canonical_key),
                        '^(.{8})(.{4})(.{4})(.{4})(.{12})$',
                        '\1-\2-\3-\4-\5'
                    )::uuid
                ) AS id,
                rc.canonical_key,
                rc.name,
                rc.group_name,
                rc.logo_url,
                c.id AS existing_id,
                c.name AS existing_name,
                c.group_name AS existing_group_name,
                c.logo_url AS existing_logo_url,
                CASE
                    WHEN c.id IS NOT NULL THEN c.enabled
                    WHEN rc.group_name IS NOT NULL
                         AND EXISTS (
                             SELECT 1
                             FROM channels grouped_channel
                             WHERE grouped_channel.group_name = rc.group_name
                               AND grouped_channel.enabled
                         )
                    THEN true
                    WHEN rc.group_name IS NOT NULL
                         AND EXISTS (
                             SELECT 1
                             FROM channels grouped_channel
                             WHERE grouped_channel.group_name = rc.group_name
                         )
                    THEN false
                    ELSE true
                END AS effective_enabled,
                CASE
                    WHEN rc.preferred_number IS NOT NULL
                         AND rc.preferred_rank = 1
                         AND NOT EXISTS (
                             SELECT 1
                             FROM channels occupied
                             WHERE occupied.channel_number = rc.preferred_number
                               AND (c.id IS NULL OR occupied.id <> c.id)
                         )
                    THEN rc.preferred_number
                    WHEN c.id IS NOT NULL THEN c.channel_number
                    ELSE nextval('canonical_channel_number_seq')::text
                END AS channel_number
            FROM ranked_candidates rc
            LEFT JOIN channels c
              ON c.provider_account_id = $1
             AND c.canonical_key = rc.canonical_key
        )
        INSERT INTO channels
            (id, channel_number, name, group_name, logo_url, enabled,
             managed_by, provider_account_id, canonical_key, revision)
        SELECT id, channel_number, name, group_name, logo_url, effective_enabled,
               'automatic', $1, canonical_key, 1
        FROM channel_rows
        -- Most refreshes repeat the same provider catalog. Skip unchanged
        -- rows before the conflict path so PostgreSQL does not recheck every
        -- existing channel during a large reconciliation publication.
        WHERE existing_id IS NULL
           OR (existing_name, existing_group_name, existing_logo_url)
              IS DISTINCT FROM (name, group_name, logo_url)
        ON CONFLICT (id) DO UPDATE SET
            name = EXCLUDED.name,
            group_name = EXCLUDED.group_name,
            logo_url = EXCLUDED.logo_url,
            updated_at = now(),
            revision = channels.revision + 1
        WHERE (channels.name, channels.group_name, channels.logo_url)
              IS DISTINCT FROM
              (EXCLUDED.name, EXCLUDED.group_name, EXCLUDED.logo_url)
        ",
    )
    .bind(account_id)
    .bind(run_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);
    Ok(inserted)
}

async fn remove_stale_candidate_channel_stream_links(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run_id: Uuid,
) -> Result<i64, PersistenceError> {
    // Move existing priorities out of the incoming range before the upsert.
    // Otherwise, swapping two stream priorities can violate the immediate
    // UNIQUE (channel_id, priority) constraint during the insert.
    sqlx::query(
        r"
        UPDATE channel_streams cs
        SET priority = cs.priority + 1000000
        FROM channels c
        WHERE cs.channel_id = c.id
          AND c.provider_account_id = $1
          AND c.managed_by = 'automatic'
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?;
    let deleted: i64 = sqlx::query(
        r"
        DELETE FROM channel_streams cs
        USING channels c
        WHERE cs.channel_id = c.id
          AND c.provider_account_id = $1
          AND c.managed_by = 'automatic'
          AND NOT EXISTS (
              SELECT 1
              FROM provider_reconciliation_candidate_streams rcs
              WHERE rcs.run_id = $2
                AND rcs.canonical_key = c.canonical_key
                AND rcs.provider_stream_id = cs.provider_stream_id
          )
        ",
    )
    .bind(account_id)
    .bind(run_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);
    Ok(deleted)
}

async fn upsert_candidate_channel_stream_links(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run_id: Uuid,
) -> Result<i64, PersistenceError> {
    let affected: i64 = sqlx::query(
        r"
        WITH distinct_links AS (
            SELECT DISTINCT c.id AS channel_id, rcs.provider_stream_id
            FROM provider_reconciliation_candidate_streams rcs
            JOIN channels c
              ON c.provider_account_id = $1
             AND c.managed_by = 'automatic'
             AND c.canonical_key = rcs.canonical_key
            WHERE rcs.run_id = $2
        ), ranked_links AS (
            SELECT channel_id,
                   provider_stream_id,
                   row_number() OVER (
                       PARTITION BY channel_id
                       ORDER BY provider_stream_id
                   ) - 1 AS priority
            FROM distinct_links
        )
        INSERT INTO channel_streams (channel_id, provider_stream_id, priority, evidence)
        SELECT channel_id,
               provider_stream_id,
               priority,
               jsonb_build_object('reconciled', now(), 'runId', $2::text)
        FROM ranked_links
        ON CONFLICT (channel_id, provider_stream_id) DO UPDATE SET
            priority = EXCLUDED.priority,
            evidence = EXCLUDED.evidence
        WHERE (channel_streams.priority, channel_streams.evidence)
              IS DISTINCT FROM (EXCLUDED.priority, EXCLUDED.evidence)
        ",
    )
    .bind(account_id)
    .bind(run_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);
    Ok(affected)
}

async fn remove_candidate_orphaned_channels(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run_id: Uuid,
) -> Result<i64, PersistenceError> {
    let deleted: i64 = sqlx::query(
        r"
        DELETE FROM channels c
        WHERE c.provider_account_id = $1
          AND c.managed_by = 'automatic'
          AND NOT EXISTS (
              SELECT 1
              FROM provider_reconciliation_candidates rc
              WHERE rc.run_id = $2
                AND rc.canonical_key = c.canonical_key
          )
        ",
    )
    .bind(account_id)
    .bind(run_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);
    Ok(deleted)
}

async fn remove_orphaned_channels(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<i64, PersistenceError> {
    let orphans_removed: i64 = sqlx::query(
        r"
        DELETE FROM channels c
        WHERE c.provider_account_id = $1
          AND c.managed_by = 'automatic'
          AND NOT EXISTS (
              SELECT 1
              FROM provider_streams ps
              JOIN source_snapshots ss ON ss.id = ps.snapshot_id
              WHERE ss.provider_account_id = $1
                AND ss.status = 'active'
                AND ss.kind IN ('m3u', 'xtream')
                AND ps.supported
                AND COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) = c.canonical_key
          )
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);

    Ok(orphans_removed)
}

async fn remove_stale_channel_stream_links(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<i64, PersistenceError> {
    let deleted: i64 = sqlx::query(
        r"
        DELETE FROM channel_streams cs
        USING channels c
        WHERE cs.channel_id = c.id
          AND c.provider_account_id = $1
          AND c.managed_by = 'automatic'
          AND NOT EXISTS (
              SELECT 1
              FROM provider_streams ps
              JOIN source_snapshots ss ON ss.id = ps.snapshot_id
              WHERE ss.provider_account_id = $1
                AND ss.status = 'active'
                AND ss.kind IN ('m3u', 'xtream')
                AND ps.supported
                AND ps.id = cs.provider_stream_id
                AND c.canonical_key = COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key)
          )
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);

    Ok(deleted)
}

async fn upsert_channel_stream_links_batch(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    canonical_keys: &[String],
) -> Result<i64, PersistenceError> {
    let result = sqlx::query(
        r"
        INSERT INTO channel_streams (channel_id, provider_stream_id, priority, evidence)
        SELECT
            c.id,
            ps.id,
            row_number() OVER (PARTITION BY c.id ORDER BY ps.id) - 1,
            jsonb_build_object('reconciled', now())
        FROM provider_streams ps
        JOIN source_snapshots ss ON ss.id = ps.snapshot_id
        JOIN channels c
          ON c.provider_account_id = $1
         AND c.managed_by = 'automatic'
         AND c.canonical_key = COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key)
        WHERE ss.provider_account_id = $1
          AND ss.status = 'active'
          AND ss.kind IN ('m3u', 'xtream')
          AND ps.supported
          AND c.canonical_key = ANY($2)
        ON CONFLICT (channel_id, provider_stream_id) DO UPDATE SET
            priority = EXCLUDED.priority
        WHERE channel_streams.priority != EXCLUDED.priority
        ",
    )
    .bind(account_id)
    .bind(canonical_keys)
    .execute(&mut **transaction)
    .await?;

    Ok(result.rows_affected().try_into().unwrap_or(i64::MAX))
}

/// A row of stream health data returned by the health check listing query.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct StreamHealthRow {
    pub provider_stream_id: Uuid,
    pub stream_name: String,
    pub group_name: Option<String>,
    pub health_status: String,
    pub health_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub health_error: Option<String>,
    pub video_codec: Option<String>,
    pub video_resolution: Option<String>,
    pub video_width: Option<i32>,
    pub video_height: Option<i32>,
    pub video_fps: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<i32>,
    pub audio_sample_rate: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub provider_account_id: Uuid,
}

/// A paginated page of stream health rows.
#[derive(Clone, Debug, Serialize)]
pub struct StreamHealthPage {
    pub total: i64,
    pub items: Vec<StreamHealthRow>,
    pub estimated: bool,
}

/// Input data for updating stream health after a probe.
#[derive(Clone, Debug)]
pub struct StreamHealthUpdate {
    pub provider_stream_id: Uuid,
    pub status: String,
    pub error: Option<String>,
    pub video_codec: Option<String>,
    pub video_resolution: Option<String>,
    pub video_width: Option<i32>,
    pub video_height: Option<i32>,
    pub video_fps: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<i32>,
    pub audio_sample_rate: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub check_duration_ms: Option<i32>,
}

/// A candidate stream for quality ranking and failover.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct ChannelStreamCandidateRow {
    pub channel_id: Uuid,
    pub channel_name: String,
    pub provider_stream_id: Uuid,
    pub stream_name: String,
    pub priority: i32,
    pub quality_rank: i32,
    pub health_status: String,
    pub video_width: Option<i32>,
    pub video_height: Option<i32>,
    pub video_fps: Option<f64>,
    pub video_codec: Option<String>,
    pub failover_count: i32,
    pub last_failover_at: Option<chrono::DateTime<chrono::Utc>>,
    pub url_template: String,
    pub provider_account_id: Uuid,
}

impl CatalogRepository {
    /// Lists provider streams with health data, filterable by status.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_stream_health(
        &self,
        status_filter: Option<&str>,
        group_filter: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<StreamHealthPage, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let offset = offset.max(0);

        // When no filter is applied, use the planner's row estimate from
        // pg_class.reltuples instead of counting all 6.8M rows. When a
        // filter is supplied, the filtered subset is small enough for an
        // exact count using the health_status/group_name partial index.
        let (total, estimated) = if status_filter.is_none() && group_filter.is_none() {
            let estimate: Option<i64> = sqlx::query_scalar(
                r"
                SELECT reltuples::bigint
                FROM pg_class
                WHERE relname = 'provider_streams'
                  AND relkind = 'r'
                ",
            )
            .fetch_one(&self.pool)
            .await?;
            (estimate.unwrap_or(0), true)
        } else {
            let count: i64 = sqlx::query_scalar(
                r"
                SELECT count(*) FROM provider_streams ps
                WHERE ($1::text IS NULL OR ps.health_status = $1)
                  AND ($2::text IS NULL OR ps.group_name = $2)
                ",
            )
            .bind(status_filter)
            .bind(group_filter)
            .fetch_one(&self.pool)
            .await?;
            (count, false)
        };

        let rows: Vec<StreamHealthRow> = sqlx::query_as(
            r"
            SELECT ps.id AS provider_stream_id, ps.name AS stream_name,
                   ps.group_name, ps.health_status, ps.health_checked_at,
                   ps.health_error, ps.video_codec, ps.video_resolution,
                   ps.video_width, ps.video_height,
                   ps.video_fps::double precision AS video_fps,
                   ps.audio_codec, ps.audio_channels, ps.audio_sample_rate,
                   ps.bitrate_kbps, ps.provider_account_id
            FROM provider_streams ps
            WHERE ($1::text IS NULL OR ps.health_status = $1)
              AND ($2::text IS NULL OR ps.group_name = $2)
            ORDER BY ps.health_status, ps.name
            LIMIT $3 OFFSET $4
            ",
        )
        .bind(status_filter)
        .bind(group_filter)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(StreamHealthPage {
            total,
            items: rows,
            estimated,
        })
    }

    /// Updates stream health data after a probe completes.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_stream_health(
        &self,
        update: &StreamHealthUpdate,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r"
            UPDATE provider_streams SET
                health_status = $2,
                health_checked_at = now(),
                health_error = $3,
                video_codec = $4,
                video_resolution = $5,
                video_width = $6,
                video_height = $7,
                video_fps = $8,
                audio_codec = $9,
                audio_channels = $10,
                audio_sample_rate = $11,
                bitrate_kbps = $12
            WHERE id = $1
            ",
        )
        .bind(update.provider_stream_id)
        .bind(&update.status)
        .bind(update.error.as_deref())
        .bind(update.video_codec.as_deref())
        .bind(update.video_resolution.as_deref())
        .bind(update.video_width)
        .bind(update.video_height)
        .bind(update.video_fps)
        .bind(update.audio_codec.as_deref())
        .bind(update.audio_channels)
        .bind(update.audio_sample_rate)
        .bind(update.bitrate_kbps)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r"
            INSERT INTO stream_health_checks (
                id, provider_stream_id, status, error_message,
                video_codec, video_resolution, video_width, video_height,
                video_fps, audio_codec, audio_channels, audio_sample_rate,
                bitrate_kbps, check_duration_ms
            ) VALUES (
                gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
            )
            ",
        )
        .bind(update.provider_stream_id)
        .bind(&update.status)
        .bind(update.error.as_deref())
        .bind(update.video_codec.as_deref())
        .bind(update.video_resolution.as_deref())
        .bind(update.video_width)
        .bind(update.video_height)
        .bind(update.video_fps)
        .bind(update.audio_codec.as_deref())
        .bind(update.audio_channels)
        .bind(update.audio_sample_rate)
        .bind(update.bitrate_kbps)
        .bind(update.check_duration_ms)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Lists streams that need health checking (unknown or dead status).
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn mark_streams_checking(
        &self,
        provider_stream_ids: &[Uuid],
    ) -> Result<u64, PersistenceError> {
        if provider_stream_ids.is_empty() {
            return Ok(0);
        }
        let rows = sqlx::query(
            r"
            UPDATE provider_streams SET
                health_status = 'checking',
                health_checked_at = now()
            WHERE id = ANY($1)
            ",
        )
        .bind(provider_stream_ids)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows)
    }

    /// Atomically transitions one stream from `unknown` or `dead` to
    /// `checking`.
    ///
    /// The probe worker calls this before it runs a probe so two workers cannot
    /// probe the same stream at the same time. Returns `true` when this call
    /// claimed the stream, and `false` when another worker already owns the
    /// `checking` transition or the stream is `alive`.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn try_mark_stream_checking(
        &self,
        provider_stream_id: Uuid,
    ) -> Result<bool, PersistenceError> {
        let rows = sqlx::query(
            r"
            UPDATE provider_streams SET
                health_status = 'checking',
                health_checked_at = now()
            WHERE id = $1
              AND health_status IN ('unknown', 'dead')
            ",
        )
        .bind(provider_stream_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows != 0)
    }

    /// Releases a health-probe claim when admission is unavailable.
    ///
    /// The update affects only the claim made by the current probe attempt.
    #[allow(clippy::missing_errors_doc)]
    pub async fn release_stream_checking_claim(
        &self,
        provider_stream_id: Uuid,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "UPDATE provider_streams SET health_status = 'unknown' \
             WHERE id = $1 AND health_status = 'checking'",
        )
        .bind(provider_stream_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Lists streams that need health checking (unknown or dead status).
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_streams_needing_health_check(
        &self,
        limit: i64,
    ) -> Result<Vec<StreamHealthRow>, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let rows: Vec<StreamHealthRow> = sqlx::query_as(
            r"
            SELECT DISTINCT ps.id AS provider_stream_id, ps.name AS stream_name,
                   ps.group_name, ps.health_status, ps.health_checked_at,
                   ps.health_error, ps.video_codec, ps.video_resolution,
                   ps.video_width, ps.video_height,
                   ps.video_fps::double precision AS video_fps,
                   ps.audio_codec, ps.audio_channels, ps.audio_sample_rate,
                   ps.bitrate_kbps, ps.provider_account_id
            FROM provider_streams ps
            JOIN provider_accounts pa ON pa.id = ps.provider_account_id
            JOIN channel_streams cs ON cs.provider_stream_id = ps.id
            JOIN channels c ON c.id = cs.channel_id AND c.enabled
            WHERE ps.health_status IN ('unknown', 'dead')
              AND ps.supported = true
              AND pa.enabled = true
            ORDER BY ps.health_status, ps.name
            LIMIT $1
            ",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Ranks streams for a channel and updates the `quality_rank` column.
    /// Higher resolution and FPS get a lower rank value (higher priority).
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn rank_channel_streams_by_quality(
        &self,
        channel_id: Uuid,
    ) -> Result<i64, PersistenceError> {
        let result = sqlx::query(
            r"
            WITH ranked AS (
                SELECT
                    cs.provider_stream_id,
                    ROW_NUMBER() OVER (
                        ORDER BY
                            COALESCE(ps.video_width, 0) DESC,
                            COALESCE(ps.video_height, 0) DESC,
                            COALESCE(ps.video_fps, 0) DESC,
                            CASE ps.health_status
                                WHEN 'alive' THEN 0
                                WHEN 'unknown' THEN 1
                                WHEN 'dead' THEN 2
                                ELSE 3
                            END,
                            cs.priority
                    ) AS new_rank
                FROM channel_streams cs
                JOIN provider_streams ps ON ps.id = cs.provider_stream_id
                WHERE cs.channel_id = $1
            )
            UPDATE channel_streams cs
            SET quality_rank = ranked.new_rank::integer
            FROM ranked
            WHERE cs.provider_stream_id = ranked.provider_stream_id
              AND cs.channel_id = $1
            ",
        )
        .bind(channel_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected().try_into().unwrap_or(i64::MAX))
    }

    /// Ranks streams for all channels by quality.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn rank_all_channel_streams_by_quality(&self) -> Result<i64, PersistenceError> {
        let result = sqlx::query(
            r"
            WITH ranked AS (
                SELECT
                    cs.channel_id,
                    cs.provider_stream_id,
                    ROW_NUMBER() OVER (
                        PARTITION BY cs.channel_id
                        ORDER BY
                            COALESCE(ps.video_width, 0) DESC,
                            COALESCE(ps.video_height, 0) DESC,
                            COALESCE(ps.video_fps, 0) DESC,
                            CASE ps.health_status
                                WHEN 'alive' THEN 0
                                WHEN 'unknown' THEN 1
                                WHEN 'dead' THEN 2
                                ELSE 3
                            END,
                            cs.priority
                    ) AS new_rank
                FROM channel_streams cs
                JOIN provider_streams ps ON ps.id = cs.provider_stream_id
            )
            UPDATE channel_streams cs
            SET quality_rank = ranked.new_rank::integer
            FROM ranked
            WHERE cs.provider_stream_id = ranked.provider_stream_id
              AND cs.channel_id = ranked.channel_id
            ",
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected().try_into().unwrap_or(i64::MAX))
    }

    /// Gets the best available stream for a channel, considering health and quality.
    /// Returns the highest-priority alive stream, falling back to unknown, then dead.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn best_stream_for_channel(
        &self,
        channel_id: Uuid,
    ) -> Result<Option<ChannelStreamCandidateRow>, PersistenceError> {
        let row: Option<ChannelStreamCandidateRow> = sqlx::query_as(
            r"
            SELECT cs.channel_id, c.name AS channel_name,
                   cs.provider_stream_id, ps.name AS stream_name,
                   cs.priority, cs.quality_rank, ps.health_status,
                   ps.video_width, ps.video_height,
                   ps.video_fps::double precision AS video_fps,
                   ps.video_codec, cs.failover_count, cs.last_failover_at,
                   ps.url_template, ps.provider_account_id
            FROM channel_streams cs
            JOIN channels c ON c.id = cs.channel_id
            JOIN provider_streams ps ON ps.id = cs.provider_stream_id
            WHERE cs.channel_id = $1
              AND ps.supported = true
            ORDER BY
                CASE ps.health_status
                    WHEN 'alive' THEN 0
                    WHEN 'unknown' THEN 1
                    WHEN 'dead' THEN 2
                    ELSE 3
                END,
                cs.quality_rank,
                cs.priority
            LIMIT 1
            ",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Records a failover event for a channel stream.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn record_failover(
        &self,
        channel_id: Uuid,
        provider_stream_id: Uuid,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r"
            UPDATE channel_streams SET
                failover_count = failover_count + 1,
                last_failover_at = now()
            WHERE channel_id = $1 AND provider_stream_id = $2
            ",
        )
        .bind(channel_id)
        .bind(provider_stream_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Gets stream health statistics for the overview dashboard.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn stream_health_stats(&self) -> Result<StreamHealthStats, PersistenceError> {
        // Group by health_status so the planner can use the partial index
        // on provider_streams(health_status, group_name) WHERE supported.
        // This avoids a full table scan on the 6.8M-row table.
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r"
            SELECT health_status, count(*) AS count
            FROM provider_streams
            WHERE supported = true
            GROUP BY health_status
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut stats = StreamHealthStats::default();
        for (status, count) in rows {
            match status.as_str() {
                "alive" => stats.alive = count,
                "dead" => stats.dead = count,
                "unknown" => stats.unknown = count,
                "checking" => stats.checking = count,
                _ => {}
            }
        }

        Ok(stats)
    }

    /// Loads the probe target data for one provider stream.
    ///
    /// The URL ciphertext stays inside the returned row. The caller decrypts
    /// it with the master key and the provider account associated data.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn load_stream_probe_target(
        &self,
        provider_stream_id: Uuid,
    ) -> Result<Option<StreamProbeTargetRow>, PersistenceError> {
        let row = sqlx::query_as::<_, StreamProbeTargetRow>(
            r"
            SELECT
                ps.id AS provider_stream_id,
                ps.provider_account_id,
                COALESCE(pa.connection_pool_id, pa.id) AS provider_pool_id,
                COALESCE(cp.max_connections, pa.max_connections) AS max_connections,
                pa.input_adapter,
                ps.url_template,
                ps.url_secret_ciphertext
            FROM provider_streams ps
            JOIN provider_accounts pa ON pa.id = ps.provider_account_id
            LEFT JOIN connection_pools cp ON cp.id = pa.connection_pool_id
            WHERE ps.id = $1
              AND ps.supported = true
              AND pa.enabled = true
            ",
        )
        .bind(provider_stream_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Lists provider streams that need a health probe.
    ///
    /// The query selects only streams mapped to enabled channels with status
    /// `unknown` or `dead`, plus streams stuck in `checking` longer than
    /// `stranded_timeout_seconds`. The scheduler uses this to enqueue
    /// low-priority probe jobs. Streams without a channel mapping are not
    /// probed, which bounds the probe backlog to the channel count.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_streams_for_health_probe(
        &self,
        limit: i64,
        stranded_timeout_seconds: i64,
    ) -> Result<Vec<StreamHealthRow>, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let rows: Vec<StreamHealthRow> = sqlx::query_as(
            r"
            SELECT DISTINCT ps.id AS provider_stream_id, ps.name AS stream_name,
                   ps.group_name, ps.health_status, ps.health_checked_at,
                   ps.health_error, ps.video_codec, ps.video_resolution,
                   ps.video_width, ps.video_height,
                   ps.video_fps::double precision AS video_fps,
                   ps.audio_codec, ps.audio_channels, ps.audio_sample_rate,
                   ps.bitrate_kbps, ps.provider_account_id
            FROM provider_streams ps
            JOIN provider_accounts pa ON pa.id = ps.provider_account_id
            JOIN channel_streams cs ON cs.provider_stream_id = ps.id
            JOIN channels c ON c.id = cs.channel_id AND c.enabled
            WHERE ps.supported = true
              AND pa.enabled = true
              AND (
                  ps.health_status IN ('unknown', 'dead')
                  OR (ps.health_status = 'checking'
                      AND ps.health_checked_at < now() - make_interval(secs => $2))
              )
            ORDER BY ps.health_status, ps.name
            LIMIT $1
            ",
        )
        .bind(limit)
        .bind(stranded_timeout_seconds)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Resets streams stuck in `checking` status back to `unknown`.
    ///
    /// A worker that crashed mid-probe leaves a stream in `checking`. This
    /// method returns those stranded records to the probe queue after the
    /// configured timeout.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn recover_stranded_checking_streams(
        &self,
        stranded_timeout_seconds: i64,
    ) -> Result<i64, PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE provider_streams SET
                health_status = 'unknown'
            WHERE health_status = 'checking'
              AND health_checked_at < now() - make_interval(secs => $1)
            ",
        )
        .bind(stranded_timeout_seconds)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected().try_into().unwrap_or(i64::MAX))
    }

    /// Resets every provider stream in `checking` status back to `unknown`.
    ///
    /// Call this on worker startup before the job loop begins. A worker that
    /// restarted mid-probe leaves streams in `checking` forever. This method
    /// returns all such streams to the probe queue without a timeout filter.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn reset_stranded_checking_streams(&self) -> Result<i64, PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE provider_streams
            SET health_status = 'unknown'
            WHERE health_status = 'checking'
            ",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected().try_into().unwrap_or(i64::MAX))
    }

    /// Lists the channel IDs linked to one provider stream.
    ///
    /// The probe uses this to re-rank affected channels after a health update.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn channels_for_stream(
        &self,
        provider_stream_id: Uuid,
    ) -> Result<Vec<Uuid>, PersistenceError> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r"
            SELECT channel_id FROM channel_streams
            WHERE provider_stream_id = $1
            ",
        )
        .bind(provider_stream_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Updates stream health data and re-ranks every linked channel.
    ///
    /// The probe calls this after a successful or failed probe so that the
    /// alternate ranking reflects the current health status.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_stream_health_and_rank(
        &self,
        update: &StreamHealthUpdate,
    ) -> Result<i64, PersistenceError> {
        self.update_stream_health(update).await?;
        let channel_ids = self.channels_for_stream(update.provider_stream_id).await?;
        let mut ranked = 0_i64;
        for channel_id in channel_ids {
            ranked += self.rank_channel_streams_by_quality(channel_id).await?;
        }
        Ok(ranked)
    }
}

/// Stream health statistics for dashboard display.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StreamHealthStats {
    pub alive: i64,
    pub dead: i64,
    pub unknown: i64,
    pub checking: i64,
}

/// One provider stream selected for a low-priority health probe.
///
/// The `url_secret_ciphertext` field holds the encrypted full URL. The probe
/// decrypts it inside the trusted worker process and never persists the
/// plaintext.
#[derive(Clone, FromRow)]
pub struct StreamProbeTargetRow {
    pub provider_stream_id: Uuid,
    pub provider_account_id: Uuid,
    pub provider_pool_id: Uuid,
    pub max_connections: i32,
    pub input_adapter: String,
    pub url_template: String,
    pub url_secret_ciphertext: Option<Vec<u8>>,
}

impl fmt::Debug for StreamProbeTargetRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamProbeTargetRow")
            .field("provider_stream_id", &self.provider_stream_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("provider_pool_id", &self.provider_pool_id)
            .field("max_connections", &self.max_connections)
            .field("input_adapter", &self.input_adapter)
            .field("url_template", &"<redacted>")
            .field("url_secret_ciphertext", &"<redacted>")
            .finish()
    }
}

/// A user account row.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub enabled: bool,
    pub last_login_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a user.
#[derive(Clone, Debug)]
pub struct CreateUserInput {
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub role: String,
}

/// Input for updating a user.
#[derive(Clone, Debug)]
pub struct UpdateUserInput {
    pub display_name: Option<String>,
    pub password_hash: Option<String>,
    pub role: Option<String>,
    pub enabled: Option<bool>,
}

impl CatalogRepository {
    /// Lists all user accounts.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_users(&self) -> Result<Vec<UserRow>, PersistenceError> {
        let rows: Vec<UserRow> = sqlx::query_as(
            r"
            SELECT id, username, display_name, role, enabled,
                   last_login_at, created_at, updated_at
            FROM users ORDER BY username
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Gets a user by username for authentication.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<UserRow>, PersistenceError> {
        let row: Option<UserRow> = sqlx::query_as(
            r"
            SELECT id, username, display_name, role, enabled,
                   last_login_at, created_at, updated_at
            FROM users WHERE username = $1
            ",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Gets a user by ID.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn get_user(&self, id: Uuid) -> Result<Option<UserRow>, PersistenceError> {
        let row: Option<UserRow> = sqlx::query_as(
            r"
            SELECT id, username, display_name, role, enabled,
                   last_login_at, created_at, updated_at
            FROM users WHERE id = $1
            ",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Creates a new user account.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_user(&self, input: &CreateUserInput) -> Result<UserRow, PersistenceError> {
        let row: UserRow = sqlx::query_as(
            r"
            INSERT INTO users (id, username, display_name, password_hash, role)
            VALUES (gen_random_uuid(), $1, $2, $3, $4)
            RETURNING id, username, display_name, role, enabled,
                      last_login_at, created_at, updated_at
            ",
        )
        .bind(&input.username)
        .bind(&input.display_name)
        .bind(&input.password_hash)
        .bind(&input.role)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Updates a user account.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_user(
        &self,
        id: Uuid,
        input: &UpdateUserInput,
    ) -> Result<Option<UserRow>, PersistenceError> {
        let row: Option<UserRow> = sqlx::query_as(
            r"
            UPDATE users SET
                display_name = COALESCE($2, display_name),
                password_hash = COALESCE($3, password_hash),
                role = COALESCE($4, role),
                enabled = COALESCE($5, enabled),
                updated_at = now()
            WHERE id = $1
            RETURNING id, username, display_name, role, enabled,
                      last_login_at, created_at, updated_at
            ",
        )
        .bind(id)
        .bind(input.display_name.as_deref())
        .bind(input.password_hash.as_deref())
        .bind(input.role.as_deref())
        .bind(input.enabled)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Deletes a user account.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_user(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let result = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Records a user login timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn record_user_login(&self, id: Uuid) -> Result<(), PersistenceError> {
        sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Gets the password hash for a user.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn get_user_password_hash(
        &self,
        username: &str,
    ) -> Result<Option<(Uuid, String)>, PersistenceError> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, password_hash FROM users WHERE username = $1 AND enabled = true",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Grants a user access to a channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn grant_channel_access(
        &self,
        user_id: Uuid,
        channel_id: Uuid,
        granted_by: Option<Uuid>,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r"
            INSERT INTO user_channel_grants (user_id, channel_id, granted_by)
            VALUES ($1, $2, $3)
            ON CONFLICT (user_id, channel_id) DO NOTHING
            ",
        )
        .bind(user_id)
        .bind(channel_id)
        .bind(granted_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Revokes a user access to a channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn revoke_channel_access(
        &self,
        user_id: Uuid,
        channel_id: Uuid,
    ) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM user_channel_grants WHERE user_id = $1 AND channel_id = $2")
            .bind(user_id)
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Lists channels a user has access to.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_user_channels(&self, user_id: Uuid) -> Result<Vec<Uuid>, PersistenceError> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT channel_id FROM user_channel_grants WHERE user_id = $1 ORDER BY channel_id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

/// A channel alias row for name standardization.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct ChannelAliasRow {
    pub id: Uuid,
    pub canonical_name: String,
    pub alias: String,
    pub country: Option<String>,
    pub category: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a channel alias.
#[derive(Clone, Debug)]
pub struct CreateChannelAliasInput {
    pub canonical_name: String,
    pub alias: String,
    pub country: Option<String>,
    pub category: Option<String>,
}

impl CatalogRepository {
    /// Lists all channel aliases.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_channel_aliases(
        &self,
        country_filter: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(i64, Vec<ChannelAliasRow>), PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let offset = offset.max(0);

        let total: i64 = sqlx::query_scalar(
            r"
            SELECT count(*) FROM channel_aliases
            WHERE ($1::text IS NULL OR country = $1)
            ",
        )
        .bind(country_filter)
        .fetch_one(&self.pool)
        .await?;

        let rows: Vec<ChannelAliasRow> = sqlx::query_as(
            r"
            SELECT id, canonical_name, alias, country, category, created_at
            FROM channel_aliases
            WHERE ($1::text IS NULL OR country = $1)
            ORDER BY canonical_name, alias
            LIMIT $2 OFFSET $3
            ",
        )
        .bind(country_filter)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok((total, rows))
    }

    /// Resolves a channel name to its canonical form using the alias database.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn resolve_channel_alias(
        &self,
        name: &str,
    ) -> Result<Option<String>, PersistenceError> {
        let row: Option<(String,)> = sqlx::query_as(
            r"
            SELECT canonical_name FROM channel_aliases
            WHERE alias = $1 OR canonical_name = $1
            LIMIT 1
            ",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Creates a new channel alias.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_channel_alias(
        &self,
        input: &CreateChannelAliasInput,
    ) -> Result<ChannelAliasRow, PersistenceError> {
        let row: ChannelAliasRow = sqlx::query_as(
            r"
            INSERT INTO channel_aliases (id, canonical_name, alias, country, category)
            VALUES (gen_random_uuid(), $1, $2, $3, $4)
            RETURNING id, canonical_name, alias, country, category, created_at
            ",
        )
        .bind(&input.canonical_name)
        .bind(&input.alias)
        .bind(input.country.as_deref())
        .bind(input.category.as_deref())
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Deletes a channel alias.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_channel_alias(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let result = sqlx::query("DELETE FROM channel_aliases WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Gets alias statistics for the dashboard.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn channel_alias_stats(&self) -> Result<ChannelAliasStats, PersistenceError> {
        let row: (i64, i64, i64) = sqlx::query_as(
            r"
            SELECT
                count(*) AS total,
                count(DISTINCT canonical_name) AS canonical_count,
                count(DISTINCT country) FILTER (WHERE country IS NOT NULL) AS country_count
            FROM channel_aliases
            ",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(ChannelAliasStats {
            total: row.0,
            canonical_names: row.1,
            countries: row.2,
        })
    }
}

/// Channel alias statistics for dashboard display.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ChannelAliasStats {
    pub total: i64,
    pub canonical_names: i64,
    pub countries: i64,
}

/// A recording rule row.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct RecordingRuleRow {
    pub id: Uuid,
    pub name: String,
    pub channel_id: Uuid,
    pub rule_type: String,
    pub title_filter: Option<String>,
    pub category_filter: Option<String>,
    pub start_padding_minutes: i32,
    pub end_padding_minutes: i32,
    pub max_recordings: Option<i32>,
    pub keep_until: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a recording rule.
#[derive(Clone, Debug)]
pub struct CreateRecordingRuleInput {
    pub name: String,
    pub channel_id: Uuid,
    pub rule_type: String,
    pub title_filter: Option<String>,
    pub category_filter: Option<String>,
    pub start_padding_minutes: i32,
    pub end_padding_minutes: i32,
    pub max_recordings: Option<i32>,
    pub keep_until: String,
}

/// A recording instance row.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct RecordingRow {
    pub id: Uuid,
    pub rule_id: Option<Uuid>,
    pub channel_id: Uuid,
    pub programme_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: chrono::DateTime<chrono::Utc>,
    pub ends_at: chrono::DateTime<chrono::Utc>,
    pub status: String,
    pub file_path: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub duration_seconds: Option<i32>,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a recording instance.
#[derive(Clone, Debug)]
pub struct CreateRecordingInput {
    pub rule_id: Option<Uuid>,
    pub channel_id: Uuid,
    pub programme_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: chrono::DateTime<chrono::Utc>,
    pub ends_at: chrono::DateTime<chrono::Utc>,
}

impl CatalogRepository {
    /// Lists all recording rules.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_recording_rules(&self) -> Result<Vec<RecordingRuleRow>, PersistenceError> {
        let rows: Vec<RecordingRuleRow> = sqlx::query_as(
            r"
            SELECT id, name, channel_id, rule_type, title_filter, category_filter,
                   start_padding_minutes, end_padding_minutes, max_recordings, keep_until,
                   enabled, created_at, updated_at
            FROM recording_rules ORDER BY name
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates a new recording rule.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_recording_rule(
        &self,
        input: &CreateRecordingRuleInput,
    ) -> Result<RecordingRuleRow, PersistenceError> {
        let row: RecordingRuleRow = sqlx::query_as(
            r"
            INSERT INTO recording_rules (
                id, name, channel_id, rule_type, title_filter, category_filter,
                start_padding_minutes, end_padding_minutes, max_recordings, keep_until
            ) VALUES (
                gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9
            )
            RETURNING id, name, channel_id, rule_type, title_filter, category_filter,
                      start_padding_minutes, end_padding_minutes, max_recordings, keep_until,
                      enabled, created_at, updated_at
            ",
        )
        .bind(&input.name)
        .bind(input.channel_id)
        .bind(&input.rule_type)
        .bind(input.title_filter.as_deref())
        .bind(input.category_filter.as_deref())
        .bind(input.start_padding_minutes)
        .bind(input.end_padding_minutes)
        .bind(input.max_recordings)
        .bind(&input.keep_until)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Deletes a recording rule.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_recording_rule(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let result = sqlx::query("DELETE FROM recording_rules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Lists recordings with optional status filter.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_recordings(
        &self,
        status_filter: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(i64, Vec<RecordingRow>), PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let offset = offset.max(0);

        let total: i64 = sqlx::query_scalar(
            r"
            SELECT count(*) FROM recordings
            WHERE ($1::text IS NULL OR status = $1)
            ",
        )
        .bind(status_filter)
        .fetch_one(&self.pool)
        .await?;

        let rows: Vec<RecordingRow> = sqlx::query_as(
            r"
            SELECT id, rule_id, channel_id, programme_id, title, description,
                   starts_at, ends_at, status, file_path, file_size_bytes,
                   duration_seconds, error_message, created_at, updated_at
            FROM recordings
            WHERE ($1::text IS NULL OR status = $1)
            ORDER BY starts_at DESC
            LIMIT $2 OFFSET $3
            ",
        )
        .bind(status_filter)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok((total, rows))
    }

    /// Creates a new recording instance.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_recording(
        &self,
        input: &CreateRecordingInput,
    ) -> Result<RecordingRow, PersistenceError> {
        let row: RecordingRow = sqlx::query_as(
            r"
            INSERT INTO recordings (
                id, rule_id, channel_id, programme_id, title, description, starts_at, ends_at
            ) VALUES (
                gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7
            )
            RETURNING id, rule_id, channel_id, programme_id, title, description,
                      starts_at, ends_at, status, file_path, file_size_bytes,
                      duration_seconds, error_message, created_at, updated_at
            ",
        )
        .bind(input.rule_id)
        .bind(input.channel_id)
        .bind(input.programme_id)
        .bind(&input.title)
        .bind(input.description.as_deref())
        .bind(input.starts_at)
        .bind(input.ends_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Updates a recording status.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_recording_status(
        &self,
        id: Uuid,
        status: &str,
        file_path: Option<&str>,
        file_size_bytes: Option<i64>,
        duration_seconds: Option<i32>,
        error_message: Option<&str>,
    ) -> Result<Option<RecordingRow>, PersistenceError> {
        let row: Option<RecordingRow> = sqlx::query_as(
            r"
            UPDATE recordings SET
                status = $2,
                file_path = COALESCE($3, file_path),
                file_size_bytes = COALESCE($4, file_size_bytes),
                duration_seconds = COALESCE($5, duration_seconds),
                error_message = $6,
                updated_at = now()
            WHERE id = $1
            RETURNING id, rule_id, channel_id, programme_id, title, description,
                      starts_at, ends_at, status, file_path, file_size_bytes,
                      duration_seconds, error_message, created_at, updated_at
            ",
        )
        .bind(id)
        .bind(status)
        .bind(file_path)
        .bind(file_size_bytes)
        .bind(duration_seconds)
        .bind(error_message)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Deletes a recording.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_recording(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let result = sqlx::query("DELETE FROM recordings WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Gets recording statistics for the dashboard.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn recording_stats(&self) -> Result<RecordingStats, PersistenceError> {
        let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
            r"
            SELECT
                count(*) FILTER (WHERE status = 'scheduled') AS scheduled,
                count(*) FILTER (WHERE status = 'recording') AS recording,
                count(*) FILTER (WHERE status = 'completed') AS completed,
                count(*) FILTER (WHERE status = 'failed') AS failed,
                COALESCE(sum(file_size_bytes) FILTER (WHERE status = 'completed'), 0)::bigint
                    AS total_bytes
            FROM recordings
            ",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(RecordingStats {
            scheduled: row.0,
            recording: row.1,
            completed: row.2,
            failed: row.3,
            total_bytes: row.4,
        })
    }
}

/// Recording statistics for dashboard display.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RecordingStats {
    pub scheduled: i64,
    pub recording: i64,
    pub completed: i64,
    pub failed: i64,
    pub total_bytes: i64,
}

/// A stream profile row.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct StreamProfileRow {
    pub id: Uuid,
    pub name: String,
    pub profile_type: String,
    pub command: Option<String>,
    pub arguments: serde_json::Value,
    pub buffer_seconds: f32,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input for creating a stream profile.
#[derive(Clone, Debug)]
pub struct CreateStreamProfileInput {
    pub name: String,
    pub profile_type: String,
    pub command: Option<String>,
    pub arguments: serde_json::Value,
    pub buffer_seconds: f32,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
}

impl CatalogRepository {
    /// Lists all stream profiles.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_stream_profiles(&self) -> Result<Vec<StreamProfileRow>, PersistenceError> {
        let rows: Vec<StreamProfileRow> = sqlx::query_as(
            r"
            SELECT id, name, profile_type, command, arguments, buffer_seconds,
                   user_agent, referer, enabled, created_at, updated_at
            FROM stream_profiles ORDER BY name
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates a new stream profile.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_stream_profile(
        &self,
        input: &CreateStreamProfileInput,
    ) -> Result<StreamProfileRow, PersistenceError> {
        let row: StreamProfileRow = sqlx::query_as(
            r"
            INSERT INTO stream_profiles (
                id, name, profile_type, command, arguments, buffer_seconds, user_agent, referer
            ) VALUES (
                gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7
            )
            RETURNING id, name, profile_type, command, arguments, buffer_seconds,
                      user_agent, referer, enabled, created_at, updated_at
            ",
        )
        .bind(&input.name)
        .bind(&input.profile_type)
        .bind(input.command.as_deref())
        .bind(&input.arguments)
        .bind(input.buffer_seconds)
        .bind(input.user_agent.as_deref())
        .bind(input.referer.as_deref())
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Deletes a stream profile.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete_stream_profile(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let result = sqlx::query("DELETE FROM stream_profiles WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Assigns a stream profile to a channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn assign_stream_profile(
        &self,
        channel_id: Uuid,
        stream_profile_id: Uuid,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r"
            INSERT INTO channel_stream_profiles (channel_id, stream_profile_id)
            VALUES ($1, $2)
            ON CONFLICT (channel_id, stream_profile_id) DO NOTHING
            ",
        )
        .bind(channel_id)
        .bind(stream_profile_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes a stream profile assignment from a channel.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn remove_stream_profile(
        &self,
        channel_id: Uuid,
        stream_profile_id: Uuid,
    ) -> Result<(), PersistenceError> {
        if stream_profile_id == Uuid::nil() {
            sqlx::query("DELETE FROM channel_stream_profiles WHERE channel_id = $1")
                .bind(channel_id)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(
                "DELETE FROM channel_stream_profiles WHERE channel_id = $1 AND stream_profile_id = $2",
            )
            .bind(channel_id)
            .bind(stream_profile_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }
    // -- Event template suggestions --

    /// Scans provider streams for event-like patterns and returns grouped
    /// suggestions for event template creation.
    ///
    /// Detects sports patterns (team vs team, team @ team) with dates, and
    /// groups them by detected league prefix (NBA, NHL, NFL, MLB, etc.) or
    /// by the group name when no league prefix is found.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn suggest_event_templates(
        &self,
    ) -> Result<Vec<EventTemplateSuggestion>, PersistenceError> {
        // Find provider streams that look like sports events: they contain
        // "vs", "vs.", "versus", "@", or "at" between two words, plus a
        // date-like pattern.
        let streams: Vec<EventSuggestionStream> = sqlx::query_as(
            r"
            SELECT ps.name, ps.group_name
            FROM provider_streams ps
            JOIN source_snapshots ss ON ss.id = ps.snapshot_id
            WHERE ss.status = 'active'
              AND ps.supported
              AND ps.name ~ '(?i)(?:vs\.?|versus|@|\bat\b)'
              AND ps.name ~ '(?i)(?:20[0-9]{2}-[0-9]{2}-[0-9]{2}|[0-9]{1,2}/[0-9]{1,2}/20[0-9]{2})'
            ORDER BY ps.name
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        // Group streams by detected league/sport prefix.
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for stream in &streams {
            let key = detect_event_group_key(&stream.name, stream.group_name.as_deref());
            groups.entry(key).or_default().push(stream.name.clone());
        }

        let mut suggestions = Vec::with_capacity(groups.len());
        for (group_key, samples) in &groups {
            let (name, display_name, match_regex, group_name) =
                build_suggestion_config(group_key, samples);
            suggestions.push(EventTemplateSuggestion {
                name,
                display_name,
                match_regex,
                channel_name_format: "{event}".to_owned(),
                group_name,
                event_duration_hours: 3,
                past_date_grace_hours: 6,
                future_date_days: 7,
                sample_streams: samples.iter().take(10).cloned().collect(),
                stream_count: u64::try_from(samples.len()).unwrap_or(u64::MAX),
            });
        }
        // Sort by stream count descending.
        suggestions.sort_by_key(|b| std::cmp::Reverse(b.stream_count));
        Ok(suggestions)
    }

    // -- Region settings --

    /// Reads the current region settings row. Creates a default row if none exists.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn get_region_settings(&self) -> Result<RegionSettingsRow, PersistenceError> {
        let row: Option<RegionSettingsRow> = sqlx::query_as(
            r"
            SELECT id, timezone, enabled_prefixes, auto_detected, created_at, updated_at
            FROM region_settings
            ORDER BY updated_at DESC
            LIMIT 1
            ",
        )
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = row {
            return Ok(row);
        }
        // Insert a default row if the table is empty.
        let row: RegionSettingsRow = sqlx::query_as(
            r#"
            INSERT INTO region_settings (timezone, enabled_prefixes, auto_detected)
            VALUES ('America/Denver', '{"US","USA","CAN","EN","LA","GLOBAL","MULTI","SPT"}', true)
            ON CONFLICT DO NOTHING
            RETURNING id, timezone, enabled_prefixes, auto_detected, created_at, updated_at
            "#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Updates the region settings row.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the update fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_region_settings(
        &self,
        timezone: &str,
        enabled_prefixes: &[String],
    ) -> Result<RegionSettingsRow, PersistenceError> {
        let row: RegionSettingsRow = sqlx::query_as(
            r"
            UPDATE region_settings SET
                timezone = $2,
                enabled_prefixes = $3,
                auto_detected = false,
                updated_at = now()
            WHERE id = (
                SELECT id FROM region_settings ORDER BY updated_at DESC LIMIT 1
            )
            RETURNING id, timezone, enabled_prefixes, auto_detected, created_at, updated_at
            ",
        )
        .bind(timezone)
        .bind(enabled_prefixes)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Lists all region prefixes found in channel group names with counts.
    ///
    /// Returns one row per distinct prefix (e.g., "US", "UK", "AF") with the
    /// number of groups and total channels that share that prefix.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_region_prefixes(&self) -> Result<Vec<RegionPrefixRow>, PersistenceError> {
        let rows: Vec<RegionPrefixRow> = sqlx::query_as(
            r"
            WITH prefixed AS (
                SELECT
                    substring(group_name FROM '^([A-Z]{2,7}):') AS prefix,
                    count(DISTINCT group_name) AS group_count,
                    count(*) AS channel_count
                FROM channels
                WHERE group_name IS NOT NULL
                  AND group_name ~ '^[A-Z]{2,7}:'
                GROUP BY prefix
            )
            SELECT prefix, group_count, channel_count
            FROM prefixed
            WHERE prefix IS NOT NULL
            ORDER BY channel_count DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Enables channels whose group name starts with one of the given prefixes
    /// and disables all other channels that have a prefixed group name.
    ///
    /// Channels without a prefixed group name are left unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the update fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn apply_region_filter(
        &self,
        enabled_prefixes: &[String],
    ) -> Result<RegionFilterStats, PersistenceError> {
        let mut transaction = self.pool.begin().await?;

        // Disable channels with prefixed groups that do not match.
        let disabled: i64 = sqlx::query(
            r"
            UPDATE channels SET enabled = false, updated_at = now()
            WHERE group_name ~ '^[A-Z]{2,7}:'
              AND enabled = true
              AND substring(group_name FROM '^([A-Z]{2,7}):') <> ALL($1::text[])
            ",
        )
        .bind(enabled_prefixes)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        // Enable channels with prefixed groups that do match.
        let enabled: i64 = sqlx::query(
            r"
            UPDATE channels SET enabled = true, updated_at = now()
            WHERE group_name ~ '^[A-Z]{2,7}:'
              AND enabled = false
              AND substring(group_name FROM '^([A-Z]{2,7}):') = ANY($1::text[])
            ",
        )
        .bind(enabled_prefixes)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        transaction.commit().await?;
        Ok(RegionFilterStats { enabled, disabled })
    }

    // -- Operator setting overrides --

    /// Loads every current operator override grouped by scope.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn load_operator_setting_overrides(
        &self,
    ) -> Result<OperatorSettingOverrides, PersistenceError> {
        let rows: Vec<OperatorSettingOverrideRow> = sqlx::query_as(
            r"
            SELECT scope, scope_id, key, value, revision, updated_by, updated_at
            FROM operator_setting_overrides
            ORDER BY scope, scope_id, key
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut overrides = OperatorSettingOverrides::default();
        for row in rows {
            overrides.set(
                &row.scope,
                &row.scope_id,
                row.key.clone(),
                row.value.clone(),
            );
        }
        Ok(overrides)
    }

    /// Loads the current overrides and revision for one scope.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn load_operator_setting_scope(
        &self,
        scope: &str,
        scope_id: &str,
    ) -> Result<OperatorSettingScopeState, PersistenceError> {
        let resource_id = operator_setting_resource_id(scope, scope_id);
        let revision: i64 = sqlx::query_scalar(
            r"
            SELECT COALESCE(MAX(revision), 0)
            FROM revisions
            WHERE resource_type = 'operator-settings' AND resource_id = $1
            ",
        )
        .bind(resource_id)
        .fetch_one(&self.pool)
        .await?;
        let rows: Vec<OperatorSettingOverrideRow> = sqlx::query_as(
            r"
            SELECT scope, scope_id, key, value, revision, updated_by, updated_at
            FROM operator_setting_overrides
            WHERE scope = $1 AND scope_id = $2
            ORDER BY key
            ",
        )
        .bind(scope)
        .bind(scope_id)
        .fetch_all(&self.pool)
        .await?;
        let mut values = BTreeMap::new();
        for row in rows {
            values.insert(row.key, row.value);
        }
        Ok(OperatorSettingScopeState { values, revision })
    }

    /// Replaces all overrides for one scope with optimistic concurrency.
    ///
    /// When `expected_revision` is `Some`, the call fails with
    /// [`PersistenceError::SettingRevisionConflict`] when the stored revision
    /// does not match. A new revision history row is recorded in the shared
    /// `revisions` table before the overrides are written.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any query fails.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn replace_operator_setting_scope(
        &self,
        scope: &str,
        scope_id: &str,
        values: &BTreeMap<String, Value>,
        expected_revision: Option<i64>,
        actor: &str,
    ) -> Result<OperatorSettingScopeState, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let resource_id = operator_setting_resource_id(scope, scope_id);

        let current_revision: i64 = sqlx::query_scalar(
            r"
            SELECT COALESCE(MAX(revision), 0)
            FROM revisions
            WHERE resource_type = 'operator-settings' AND resource_id = $1
            ",
        )
        .bind(resource_id)
        .fetch_one(&mut *transaction)
        .await?;

        if let Some(expected) = expected_revision
            && expected != current_revision
        {
            transaction.rollback().await.ok();
            return Err(PersistenceError::SettingRevisionConflict {
                expected,
                actual: current_revision,
            });
        }

        let before_value =
            current_operator_scope_snapshot(&mut transaction, scope, scope_id, current_revision)
                .await?;
        let after_value = serde_json::to_value(values).map_err(|_| {
            PersistenceError::Database(sqlx::Error::Protocol("encode overrides".to_owned()))
        })?;
        let next_revision = current_revision + 1;

        sqlx::query(
            r"
            INSERT INTO revisions (id, resource_type, resource_id, revision, actor, before_value, after_value)
            VALUES ($1, 'operator-settings', $2, $3, $4, $5, $6)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(resource_id)
        .bind(next_revision)
        .bind(actor)
        .bind(before_value)
        .bind(&after_value)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r"
            DELETE FROM operator_setting_overrides WHERE scope = $1 AND scope_id = $2
            ",
        )
        .bind(scope)
        .bind(scope_id)
        .execute(&mut *transaction)
        .await?;

        for (key, value) in values {
            sqlx::query(
                r"
                INSERT INTO operator_setting_overrides
                    (scope, scope_id, key, value, revision, updated_by)
                VALUES ($1, $2, $3, $4, $5, $6)
                ",
            )
            .bind(scope)
            .bind(scope_id)
            .bind(key)
            .bind(value)
            .bind(next_revision)
            .bind(actor)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(OperatorSettingScopeState {
            values: values.clone(),
            revision: next_revision,
        })
    }

    /// Lists revision history rows for one scope, newest first.
    /// The limit is clamped to the shared maximum page size.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_operator_setting_revisions(
        &self,
        scope: &str,
        scope_id: &str,
        limit: i64,
    ) -> Result<Vec<OperatorSettingRevisionRow>, PersistenceError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let resource_id = operator_setting_resource_id(scope, scope_id);
        let rows: Vec<OperatorSettingRevisionRow> = sqlx::query_as(
            r"
            SELECT revision, actor, before_value, after_value, created_at
            FROM revisions
            WHERE resource_type = 'operator-settings' AND resource_id = $1
            ORDER BY revision DESC
            LIMIT $2
            ",
        )
        .bind(resource_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Restores the overrides captured at `target_revision` and records a new
    /// revision that marks the restore.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::SettingRevisionNotFound`] when the target
    /// revision does not exist for the scope.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn rollback_operator_setting_scope(
        &self,
        scope: &str,
        scope_id: &str,
        target_revision: i64,
        actor: &str,
    ) -> Result<OperatorSettingScopeState, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let resource_id = operator_setting_resource_id(scope, scope_id);

        let target: Option<(Option<Value>, Value)> = sqlx::query_as(
            r"
            SELECT before_value, after_value
            FROM revisions
            WHERE resource_type = 'operator-settings' AND resource_id = $1 AND revision = $2
            ",
        )
        .bind(resource_id)
        .bind(target_revision)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((_, after_value)) = target else {
            transaction.rollback().await.ok();
            return Err(PersistenceError::SettingRevisionNotFound {
                scope: scope.to_owned(),
                scope_id: scope_id.to_owned(),
                target: target_revision,
            });
        };

        let restored: BTreeMap<String, Value> =
            serde_json::from_value(after_value).map_err(|_| {
                PersistenceError::Database(sqlx::Error::Protocol("decode overrides".to_owned()))
            })?;

        let current_revision: i64 = sqlx::query_scalar(
            r"
            SELECT COALESCE(MAX(revision), 0)
            FROM revisions
            WHERE resource_type = 'operator-settings' AND resource_id = $1
            ",
        )
        .bind(resource_id)
        .fetch_one(&mut *transaction)
        .await?;

        let before_value =
            current_operator_scope_snapshot(&mut transaction, scope, scope_id, current_revision)
                .await?;
        let next_revision = current_revision + 1;
        let after_value = serde_json::to_value(&restored).map_err(|_| {
            PersistenceError::Database(sqlx::Error::Protocol("encode overrides".to_owned()))
        })?;

        sqlx::query(
            r"
            INSERT INTO revisions (id, resource_type, resource_id, revision, actor, before_value, after_value)
            VALUES ($1, 'operator-settings', $2, $3, $4, $5, $6)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(resource_id)
        .bind(next_revision)
        .bind(actor)
        .bind(before_value)
        .bind(&after_value)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            r"
            DELETE FROM operator_setting_overrides WHERE scope = $1 AND scope_id = $2
            ",
        )
        .bind(scope)
        .bind(scope_id)
        .execute(&mut *transaction)
        .await?;

        for (key, value) in &restored {
            sqlx::query(
                r"
                INSERT INTO operator_setting_overrides
                    (scope, scope_id, key, value, revision, updated_by)
                VALUES ($1, $2, $3, $4, $5, $6)
                ",
            )
            .bind(scope)
            .bind(scope_id)
            .bind(key)
            .bind(value)
            .bind(next_revision)
            .bind(actor)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(OperatorSettingScopeState {
            values: restored,
            revision: next_revision,
        })
    }
}

/// One reconciliation revision history row for a provider account.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ReconciliationRevisionRow {
    pub revision: i64,
    pub actor: String,
    pub before_value: Option<Value>,
    pub after_value: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Counts produced by a reconciliation rollback.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReconciliationRollbackStats {
    pub target_revision: i64,
    pub channels_removed: i64,
    pub channels_restored: i64,
    pub stream_links_restored: i64,
    pub epg_mappings_restored: i64,
}

/// One automatic channel captured in a reconciliation snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, FromRow)]
struct SnapshotChannel {
    id: Uuid,
    channel_number: String,
    name: String,
    group_name: Option<String>,
    logo_url: Option<String>,
    enabled: bool,
    canonical_key: Option<String>,
}

/// One stream link captured in a reconciliation snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, FromRow)]
struct SnapshotStreamLink {
    channel_id: Uuid,
    provider_stream_id: Uuid,
    priority: i32,
    evidence: Value,
}

/// One EPG mapping captured in a reconciliation snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, FromRow)]
struct SnapshotEpgMapping {
    channel_id: Uuid,
    epg_channel_id: Uuid,
    method: String,
    confidence: f32,
    evidence: Value,
    review_status: String,
    reviewed_by: Option<String>,
    reviewed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The captured automatic channel, stream link, and EPG mapping state for
/// one provider account. Stored as the `before_value` or `after_value` of a
/// reconciliation revision row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct ReconciliationSnapshot {
    channels: Vec<SnapshotChannel>,
    stream_links: Vec<SnapshotStreamLink>,
    epg_mappings: Vec<SnapshotEpgMapping>,
}

/// Reads the automatic channels, their stream links, and their EPG mappings
/// for one provider account as a JSON snapshot.
async fn capture_reconciliation_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<Option<Value>, PersistenceError> {
    let channels: Vec<SnapshotChannel> = sqlx::query_as(
        r"
        SELECT id, channel_number, name, group_name, logo_url, enabled, canonical_key
        FROM channels
        WHERE provider_account_id = $1 AND managed_by = 'automatic'
        ORDER BY id
        ",
    )
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await?;
    let channel_ids: Vec<Uuid> = channels.iter().map(|channel| channel.id).collect();
    let stream_links: Vec<SnapshotStreamLink> = if channel_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            r"
            SELECT channel_id, provider_stream_id, priority, evidence
            FROM channel_streams
            WHERE channel_id = ANY($1)
            ORDER BY channel_id, provider_stream_id
            ",
        )
        .bind(&channel_ids)
        .fetch_all(&mut **transaction)
        .await?
    };
    let epg_mappings: Vec<SnapshotEpgMapping> = if channel_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            r"
            SELECT channel_id, epg_channel_id, method, confidence, evidence,
                   review_status, reviewed_by, reviewed_at
            FROM channel_epg_mappings
            WHERE channel_id = ANY($1)
            ORDER BY channel_id
            ",
        )
        .bind(&channel_ids)
        .fetch_all(&mut **transaction)
        .await?
    };
    let snapshot = ReconciliationSnapshot {
        channels,
        stream_links,
        epg_mappings,
    };
    let payload = serde_json::to_value(&snapshot).map_err(|_| {
        PersistenceError::Database(sqlx::Error::Protocol(
            "encode reconciliation snapshot".to_owned(),
        ))
    })?;
    Ok(Some(payload))
}

/// Returns the latest reconciliation revision number for one account, or 0
/// when no reconciliation has been recorded yet.
async fn current_reconciliation_revision(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<i64, PersistenceError> {
    let revision: i64 = sqlx::query_scalar(
        r"
        SELECT COALESCE(MAX(revision), 0)
        FROM revisions
        WHERE resource_type = 'reconciliation' AND resource_id = $1
        ",
    )
    .bind(account_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(revision)
}

/// Writes one reconciliation revision row with before and after snapshots.
async fn record_reconciliation_revision(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    revision: i64,
    actor: &str,
    before_value: Option<Value>,
    after_value: Option<Value>,
) -> Result<(), PersistenceError> {
    sqlx::query(
        r"
        INSERT INTO revisions (id, resource_type, resource_id, revision, actor, before_value, after_value)
        VALUES ($1, 'reconciliation', $2, $3, $4, $5, $6)
        ",
    )
    .bind(Uuid::now_v7())
    .bind(account_id)
    .bind(revision)
    .bind(actor)
    .bind(before_value)
    .bind(after_value)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Returns true when no other channel currently holds the given channel
/// number. The channel being restored may already have that number.
async fn channel_number_available_for(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    channel_number: &str,
    channel_id: Uuid,
) -> Result<bool, PersistenceError> {
    let taken: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channels WHERE channel_number = $1 AND id <> $2")
            .bind(channel_number)
            .bind(channel_id)
            .fetch_one(&mut **transaction)
            .await?;
    Ok(taken == 0)
}

/// Allocates the next channel number from the canonical sequence.
async fn next_channel_number(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<String, PersistenceError> {
    let number: String = sqlx::query_scalar("SELECT nextval('canonical_channel_number_seq')::text")
        .fetch_one(&mut **transaction)
        .await?;
    Ok(number)
}

/// Deterministic namespace UUID for operator setting revision identity.
const OPERATOR_SETTING_NAMESPACE: Uuid = Uuid::from_u128(0x9d4f_5e2a_1c6b_4d8a_9e7f_3a2c_1b5d_8f04);

/// Builds a stable `UUIDv5` resource id for one operator setting scope.
fn operator_setting_resource_id(scope: &str, scope_id: &str) -> Uuid {
    let mut material = String::with_capacity(scope.len() + scope_id.len() + 1);
    material.push_str(scope);
    material.push(':');
    material.push_str(scope_id);
    Uuid::new_v5(&OPERATOR_SETTING_NAMESPACE, material.as_bytes())
}

/// Reads the current overrides for a scope as a JSON object snapshot.
async fn current_operator_scope_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &str,
    scope_id: &str,
    revision: i64,
) -> Result<Option<Value>, PersistenceError> {
    let rows: Vec<(String, Value)> = sqlx::query_as(
        r"
        SELECT key, value FROM operator_setting_overrides
        WHERE scope = $1 AND scope_id = $2
        ORDER BY key
        ",
    )
    .bind(scope)
    .bind(scope_id)
    .fetch_all(&mut **transaction)
    .await?;
    let mut object = serde_json::Map::new();
    for (key, value) in rows {
        object.insert(key, value);
    }
    let snapshot = Value::Object(object);
    let payload = serde_json::json!({
        "revision": revision,
        "overrides": snapshot,
    });
    Ok(Some(payload))
}

/// Current overrides and revision for one operator setting scope.
#[derive(Clone, Debug)]
pub struct OperatorSettingScopeState {
    pub values: BTreeMap<String, Value>,
    pub revision: i64,
}

/// All current operator overrides grouped by scope.
#[derive(Clone, Debug, Default)]
pub struct OperatorSettingOverrides {
    pub global: BTreeMap<String, Value>,
    pub providers: BTreeMap<String, BTreeMap<String, Value>>,
    pub groups: BTreeMap<String, BTreeMap<String, Value>>,
}

impl OperatorSettingOverrides {
    fn set(&mut self, scope: &str, scope_id: &str, key: String, value: Value) {
        match scope {
            "global" => {
                self.global.insert(key, value);
            }
            "provider" => {
                self.providers
                    .entry(scope_id.to_owned())
                    .or_default()
                    .insert(key, value);
            }
            "group" => {
                self.groups
                    .entry(scope_id.to_owned())
                    .or_default()
                    .insert(key, value);
            }
            _ => {}
        }
    }

    /// Builds the domain override map for one provider and group.
    #[must_use]
    pub fn to_domain(&self, provider_id: &str, group_id: &str) -> iptv_domain::SettingOverrides {
        let provider = self.providers.get(provider_id).cloned().unwrap_or_default();
        let group = self.groups.get(group_id).cloned().unwrap_or_default();
        iptv_domain::SettingOverrides {
            global: self.global.clone(),
            provider,
            group,
        }
    }
}

/// One persisted operator override row.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct OperatorSettingOverrideRow {
    pub scope: String,
    pub scope_id: String,
    pub key: String,
    pub value: Value,
    pub revision: i64,
    pub updated_by: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One revision history row for an operator setting scope.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct OperatorSettingRevisionRow {
    pub revision: i64,
    pub actor: String,
    pub before_value: Option<Value>,
    pub after_value: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Region settings row stored in the `region_settings` table.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RegionSettingsRow {
    pub id: Uuid,
    pub timezone: String,
    pub enabled_prefixes: Vec<String>,
    pub auto_detected: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A region prefix with group and channel counts.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RegionPrefixRow {
    pub prefix: String,
    pub group_count: i64,
    pub channel_count: i64,
}

/// Statistics from applying a region filter.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RegionFilterStats {
    pub enabled: i64,
    pub disabled: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programme_row_extracts_categories_from_json_array() {
        let row = ProgrammeRow {
            id: "p1".to_owned(),
            channel_id: None,
            channel_name: "News".to_owned(),
            title: "Bulletin".to_owned(),
            subtitle: None,
            description: None,
            categories: serde_json::json!(["News", "Politics"]),
            starts_at: Utc::now(),
            stops_at: Utc::now(),
            source_name: None,
        };
        assert_eq!(
            row.category_list(),
            vec!["News".to_owned(), "Politics".to_owned()]
        );
    }

    #[test]
    fn programme_row_tolerates_non_array_categories() {
        let row = ProgrammeRow {
            id: "p2".to_owned(),
            channel_id: None,
            channel_name: "News".to_owned(),
            title: "Bulletin".to_owned(),
            subtitle: None,
            description: None,
            categories: serde_json::json!({}),
            starts_at: Utc::now(),
            stops_at: Utc::now(),
            source_name: None,
        };
        assert!(row.category_list().is_empty());
    }

    #[test]
    fn page_size_clamps_are_applied_by_callers() {
        assert_eq!(DEFAULT_PAGE_SIZE, 100);
        assert_eq!(MAX_PAGE_SIZE, 500);
    }

    #[test]
    fn playback_candidate_debug_redacts_provider_values() {
        let channel_id = Uuid::now_v7();
        let candidate = ChannelPlaybackCandidateRow {
            alternative_base_urls: serde_json::json!([]),
            channel_id,
            channel_name: "News".to_owned(),
            channel_revision: 4,
            provider_stream_id: Uuid::now_v7(),
            source_snapshot_id: Uuid::now_v7(),
            provider_account_id: Uuid::now_v7(),
            provider_revision: 2,
            provider_pool_id: Uuid::now_v7(),
            max_connections: 3,
            input_adapter: "native-ts".to_owned(),
            stream_url: "https://provider.test/live?password=secret".to_owned(),
            url_secret_ciphertext: Some(b"secret ciphertext".to_vec()),
            priority: 1,
            quality_rank: 2,
            health_status: "alive".to_owned(),
            bitrate_kbps: Some(8_000),
        };
        let rendered = format!("{candidate:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains(&channel_id.to_string()));
        assert!(!rendered.contains("provider.test"));
        assert!(!rendered.contains("secret ciphertext"));
    }

    #[test]
    fn output_profile_token_hash_debug_is_redacted() {
        let hash = OutputProfileTokenHash::from_sha256([0xabu8; 32]);
        let rendered = format!("{hash:?}");
        assert_eq!(rendered, "OutputProfileTokenHash(<redacted>)");
        assert!(!rendered.contains("ab"));
    }
}
