//! Durable dispatch for staged provider work.
use crate::{CatalogRepository, JobRepository, PersistenceError};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub(crate) async fn queue_reconciliation(
    transaction: &mut Transaction<'_, Postgres>,
    run: &crate::ProviderReconciliationRun,
) -> Result<(), PersistenceError> {
    let Some(parent) = run.parent_job_id else {
        return Ok(());
    };
    for partition in 0..run.partition_count {
        let payload = serde_json::json!({
            "runId": run.id.to_string(), "partitionNumber": partition.to_string(),
            "sourceId": run.provider_account_id.to_string(),
            "parentJobId": parent.to_string(),
        });
        sqlx::query("INSERT INTO job_outbox (id, parent_job_id, kind, payload, dedup_key) VALUES ($1, $2, 'reconcile-provider-partition', $3, $4) ON CONFLICT (dedup_key) DO NOTHING")
            .bind(Uuid::now_v7()).bind(parent).bind(payload)
            .bind(format!("partition:{}:{}:{partition}", parent, run.id))
            .execute(&mut **transaction).await?;
    }
    Ok(())
}

impl JobRepository {
    /// Dispatches bounded durable work after a commit or worker restart.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn dispatch_outbox(&self) -> Result<u64, PersistenceError> {
        // Preparation is idempotent. Do not hold an outbox lock while the
        // catalog transaction locks a snapshot and creates child intents.
        let preparations: Vec<(Uuid, Uuid, Value)> = sqlx::query_as(
            "SELECT o.id, o.parent_job_id, o.payload FROM job_outbox o JOIN jobs j ON j.id = o.parent_job_id WHERE o.dispatched_at IS NULL AND o.kind = 'prepare-provider-reconciliation' AND j.status IN ('queued', 'running') ORDER BY o.created_at LIMIT 16",
        ).fetch_all(&self.pool).await?;
        let catalog = CatalogRepository::new(self.pool.clone());
        for (id, parent, payload) in preparations {
            let snapshot = payload
                .get("snapshotId")
                .and_then(Value::as_str)
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or_else(|| {
                    PersistenceError::InvalidSource("invalid staging dispatch snapshot".to_owned())
                })?;
            let partitions = payload
                .get("partitionCount")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(3);
            catalog
                .create_or_load_provider_reconciliation_run(snapshot, parent, partitions)
                .await?;
            sqlx::query("UPDATE job_outbox SET dispatched_at = now() WHERE id = $1 AND dispatched_at IS NULL")
                .bind(id).execute(&self.pool).await?;
        }
        let entries: Vec<(Uuid, Uuid, String, Value, i32)> = sqlx::query_as(
            "SELECT id, parent_job_id, kind, payload, priority FROM job_outbox o WHERE dispatched_at IS NULL AND kind <> 'prepare-provider-reconciliation' AND (kind <> 'finalize-provider-reconciliation' OR NOT EXISTS (SELECT 1 FROM provider_reconciliation_partitions p WHERE p.run_id::text = o.payload->>'runId' AND p.status <> 'succeeded')) ORDER BY created_at LIMIT 128",
        ).fetch_all(&self.pool).await?;
        let mut dispatched = 0;
        for (id, parent, kind, payload, priority) in entries {
            let mut transaction = self.pool.begin().await?;
            // The parent lock serializes dispatch with source cancellation.
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1 FOR UPDATE")
                    .bind(parent)
                    .fetch_optional(&mut *transaction)
                    .await?;
            let pending: Option<i32> = sqlx::query_scalar("SELECT 1 FROM job_outbox WHERE id = $1 AND dispatched_at IS NULL FOR UPDATE SKIP LOCKED")
                .bind(id).fetch_optional(&mut *transaction).await?;
            if pending.is_none() {
                transaction.commit().await?;
                continue;
            }
            if status
                .as_deref()
                .is_some_and(|s| matches!(s, "queued" | "running"))
            {
                sqlx::query("INSERT INTO jobs (id, kind, payload, priority, available_at) VALUES ($1, $2, $3, $4, now()) ON CONFLICT DO NOTHING")
                    .bind(id).bind(kind).bind(payload).bind(priority)
                    .execute(&mut *transaction).await?;
                dispatched += 1;
            }
            sqlx::query("UPDATE job_outbox SET dispatched_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
        }
        Ok(dispatched)
    }
}
