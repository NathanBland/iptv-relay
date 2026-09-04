use crate::{
    IngestError, PreparedEpgChannel, PreparedProgramme, PreparedProviderStream, PreparedSnapshot,
    SnapshotOwner, StagedRows,
};
use sqlx::{PgPool, Postgres, QueryBuilder};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

const PROVIDER_STREAM_BATCH: usize = 500;
const EPG_CHANNEL_BATCH: usize = 1_000;
const PROGRAMME_BATCH: usize = 1_000;

/// Reports each completed snapshot write batch.
///
/// The caller persists these reports outside the snapshot transaction. A
/// cancelled report returns an error and rolls back the transaction.
pub trait ActivationProgress: Send + Sync {
    #[allow(clippy::missing_errors_doc)]
    fn checkpoint(
        &self,
        records_staged: u64,
        records_total: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), IngestError>> + '_>>;
}

#[derive(Debug)]
struct NoopActivationProgress;

impl ActivationProgress for NoopActivationProgress {
    fn checkpoint(
        &self,
        _records_staged: u64,
        _records_total: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), IngestError>> + '_>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Debug)]
pub struct PgSnapshotStore {
    pool: PgPool,
}

impl PgSnapshotStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Stores a complete snapshot with `staging` status.
    ///
    /// The transaction commits before a worker reads the snapshot. Active
    /// provider and guide data remains unchanged until `activate_staged`.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn stage(&self, snapshot: &PreparedSnapshot) -> Result<Uuid, IngestError> {
        self.stage_with_progress(snapshot, &NoopActivationProgress)
            .await
    }

    /// Stores one immutable snapshot and reports each completed write batch.
    ///
    /// A failed checkpoint rolls back the staging transaction. A successful
    /// return means that workers can read the rows from a new transaction.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn stage_with_progress(
        &self,
        snapshot: &PreparedSnapshot,
        progress: &dyn ActivationProgress,
    ) -> Result<Uuid, IngestError> {
        validate_snapshot(snapshot)?;
        let byte_count = to_i64(snapshot.byte_count)?;
        let record_count = to_i64(snapshot.record_count)?;
        let diagnostic_count = to_i64(snapshot.diagnostic_count)?;
        let provider_account_id = match snapshot.owner {
            SnapshotOwner::ProviderAccount(id) => Some(id),
            SnapshotOwner::EpgSource(_) => None,
        };
        let epg_source_id = match snapshot.owner {
            SnapshotOwner::ProviderAccount(_) => None,
            SnapshotOwner::EpgSource(id) => Some(id),
        };

        let mut transaction = self.pool.begin().await?;
        if let Some((existing_id, status)) = sqlx::query_as::<_, (Uuid, String)>(
            r"
            SELECT id, status
            FROM source_snapshots
            WHERE provider_account_id IS NOT DISTINCT FROM $1
              AND epg_source_id IS NOT DISTINCT FROM $2
              AND kind = $3 AND checksum_sha256 = $4
            FOR UPDATE
            ",
        )
        .bind(provider_account_id)
        .bind(epg_source_id)
        .bind(snapshot.format.snapshot_kind())
        .bind(&snapshot.checksum_sha256)
        .fetch_optional(&mut *transaction)
        .await?
        {
            if status == "active" {
                transaction.commit().await?;
                progress
                    .checkpoint(snapshot.record_count, snapshot.record_count)
                    .await?;
                return Ok(existing_id);
            }
            if status == "staging" {
                let complete = staged_row_count(&mut transaction, existing_id).await?;
                if complete == snapshot.record_count {
                    transaction.commit().await?;
                    progress
                        .checkpoint(snapshot.record_count, snapshot.record_count)
                        .await?;
                    return Ok(existing_id);
                }
                return Err(IngestError::InvalidRequest(
                    "matching staged snapshot is incomplete; discard it before retrying",
                ));
            }
            if status == "superseded" {
                let complete = staged_row_count(&mut transaction, existing_id).await?;
                if complete != snapshot.record_count {
                    return Err(IngestError::InvalidRequest(
                        "matching superseded snapshot is incomplete",
                    ));
                }
                sqlx::query(
                    "UPDATE source_snapshots SET status = 'staging', staged_at = now(), activated_at = NULL WHERE id = $1",
                )
                .bind(existing_id)
                .execute(&mut *transaction)
                .await?;
                transaction.commit().await?;
                progress
                    .checkpoint(snapshot.record_count, snapshot.record_count)
                    .await?;
                return Ok(existing_id);
            }
            // Rejected snapshots remain audit records and cannot be reused.
            return Err(IngestError::InvalidRequest(
                "matching snapshot already exists in a terminal state",
            ));
        }

        insert_snapshot_header(
            &mut transaction,
            snapshot,
            provider_account_id,
            epg_source_id,
            byte_count,
            record_count,
            diagnostic_count,
        )
        .await?;
        let mut records_staged = 0_u64;
        insert_provider_streams(
            &mut transaction,
            snapshot.id,
            provider_account_id,
            &snapshot.provider_streams,
            progress,
            &mut records_staged,
            snapshot.record_count,
        )
        .await?;
        insert_epg_channels(
            &mut transaction,
            snapshot.id,
            provider_account_id,
            epg_source_id,
            snapshot.format.snapshot_kind(),
            &snapshot.epg_channels,
            progress,
            &mut records_staged,
            snapshot.record_count,
        )
        .await?;
        insert_programmes(
            &mut transaction,
            snapshot.id,
            &snapshot.programmes,
            progress,
            &mut records_staged,
            snapshot.record_count,
        )
        .await?;
        if records_staged != snapshot.record_count {
            return Err(IngestError::InvalidRequest(
                "snapshot staging count does not match record count",
            ));
        }
        sqlx::query("UPDATE source_snapshots SET staged_at = now() WHERE id = $1")
            .bind(snapshot.id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(snapshot.id)
    }

    /// Publishes a complete staged snapshot in one transaction.
    ///
    /// The previous active snapshot remains visible until this transaction
    /// commits. The method is idempotent when the snapshot is already active.
    #[allow(clippy::missing_errors_doc)]
    pub async fn activate_staged(&self, snapshot_id: Uuid) -> Result<Uuid, IngestError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query_as::<
            _,
            (
                Option<Uuid>,
                Option<Uuid>,
                String,
                String,
                i64,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(
            r"
            SELECT provider_account_id, epg_source_id, kind, status,
                   record_count, staged_at
            FROM source_snapshots
            WHERE id = $1
            FOR UPDATE
            ",
        )
        .bind(snapshot_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(IngestError::InvalidRequest("staged snapshot was not found"))?;
        let (provider_account_id, epg_source_id, kind, status, record_count, staged_at) = row;
        if status == "active" {
            transaction.commit().await?;
            return Ok(snapshot_id);
        }
        if status != "staging" || staged_at.is_none() {
            return Err(IngestError::InvalidRequest(
                "snapshot is not ready for activation",
            ));
        }
        let actual_count = staged_row_count(&mut transaction, snapshot_id).await?;
        if actual_count != u64::try_from(record_count).unwrap_or(u64::MAX) {
            return Err(IngestError::InvalidRequest(
                "staged snapshot record count does not match its header",
            ));
        }
        supersede_current(
            &mut transaction,
            provider_account_id,
            epg_source_id,
            &kind,
            snapshot_id,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE source_snapshots SET status = 'active', activated_at = now() WHERE id = $1 AND status = 'staging'",
        )
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(IngestError::InvalidRequest(
                "staged snapshot was not activatable",
            ));
        }
        transaction.commit().await?;
        Ok(snapshot_id)
    }

    /// Removes an incomplete staged snapshot without changing active data.
    #[allow(clippy::missing_errors_doc)]
    pub async fn discard_staged(&self, snapshot_id: Uuid) -> Result<bool, IngestError> {
        let deleted =
            sqlx::query("DELETE FROM source_snapshots WHERE id = $1 AND status = 'staging'")
                .bind(snapshot_id)
                .execute(&self.pool)
                .await?;
        Ok(deleted.rows_affected() == 1)
    }

    /// Inserts a complete immutable snapshot and activates it in one transaction.
    /// Any failure rolls back staging and leaves the previous active snapshot untouched.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn activate(&self, snapshot: &PreparedSnapshot) -> Result<Uuid, IngestError> {
        self.activate_with_progress(snapshot, &NoopActivationProgress)
            .await
    }

    /// Inserts one snapshot and reports each completed database write batch.
    ///
    /// A report failure rolls back the snapshot transaction. The reports do
    /// not make staged rows visible before the transaction commits.
    #[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
    pub async fn activate_with_progress(
        &self,
        snapshot: &PreparedSnapshot,
        progress: &dyn ActivationProgress,
    ) -> Result<Uuid, IngestError> {
        validate_snapshot(snapshot)?;
        let byte_count = to_i64(snapshot.byte_count)?;
        let record_count = to_i64(snapshot.record_count)?;
        let diagnostic_count = to_i64(snapshot.diagnostic_count)?;
        let provider_account_id = match snapshot.owner {
            SnapshotOwner::ProviderAccount(id) => Some(id),
            SnapshotOwner::EpgSource(_) => None,
        };
        let epg_source_id = match snapshot.owner {
            SnapshotOwner::ProviderAccount(_) => None,
            SnapshotOwner::EpgSource(id) => Some(id),
        };

        let mut transaction = self.pool.begin().await?;
        if let Some(existing_id) = sqlx::query_scalar::<_, Uuid>(
            r"
            SELECT id
            FROM source_snapshots
            WHERE provider_account_id IS NOT DISTINCT FROM $1
              AND epg_source_id IS NOT DISTINCT FROM $2
              AND kind = $3 AND checksum_sha256 = $4
            FOR UPDATE
            ",
        )
        .bind(provider_account_id)
        .bind(epg_source_id)
        .bind(snapshot.format.snapshot_kind())
        .bind(&snapshot.checksum_sha256)
        .fetch_optional(&mut *transaction)
        .await?
        {
            supersede_current(
                &mut transaction,
                provider_account_id,
                epg_source_id,
                snapshot.format.snapshot_kind(),
                existing_id,
            )
            .await?;
            sqlx::query(
                "UPDATE source_snapshots SET status = 'active', activated_at = now() WHERE id = $1",
            )
            .bind(existing_id)
            .execute(&mut *transaction)
            .await?;
            progress
                .checkpoint(snapshot.record_count, snapshot.record_count)
                .await?;
            transaction.commit().await?;
            return Ok(existing_id);
        }

        insert_snapshot_header(
            &mut transaction,
            snapshot,
            provider_account_id,
            epg_source_id,
            byte_count,
            record_count,
            diagnostic_count,
        )
        .await?;

        let records_total = snapshot.record_count;
        let mut records_staged = 0_u64;
        insert_provider_streams(
            &mut transaction,
            snapshot.id,
            provider_account_id,
            &snapshot.provider_streams,
            progress,
            &mut records_staged,
            records_total,
        )
        .await?;
        insert_epg_channels(
            &mut transaction,
            snapshot.id,
            provider_account_id,
            epg_source_id,
            snapshot.format.snapshot_kind(),
            &snapshot.epg_channels,
            progress,
            &mut records_staged,
            records_total,
        )
        .await?;
        insert_programmes(
            &mut transaction,
            snapshot.id,
            &snapshot.programmes,
            progress,
            &mut records_staged,
            records_total,
        )
        .await?;

        if records_staged != records_total {
            return Err(IngestError::InvalidRequest(
                "snapshot staging count does not match record count",
            ));
        }

        supersede_current(
            &mut transaction,
            provider_account_id,
            epg_source_id,
            snapshot.format.snapshot_kind(),
            snapshot.id,
        )
        .await?;
        let activated = sqlx::query(
            r"
            UPDATE source_snapshots
            SET status = 'active', activated_at = now()
            WHERE id = $1 AND status = 'staging'
            ",
        )
        .bind(snapshot.id)
        .execute(&mut *transaction)
        .await?;
        if activated.rows_affected() != 1 {
            return Err(IngestError::InvalidRequest(
                "staged snapshot was not activatable",
            ));
        }
        transaction.commit().await?;
        Ok(snapshot.id)
    }
}

fn validate_snapshot(snapshot: &PreparedSnapshot) -> Result<(), IngestError> {
    if !snapshot.is_nonempty() {
        return Err(IngestError::EmptySnapshot {
            format: snapshot.format.display_name(),
        });
    }
    validate_owner(snapshot)
}

async fn insert_snapshot_header(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot: &PreparedSnapshot,
    provider_account_id: Option<Uuid>,
    epg_source_id: Option<Uuid>,
    byte_count: i64,
    record_count: i64,
    diagnostic_count: i64,
) -> Result<(), IngestError> {
    sqlx::query(
        r"
        INSERT INTO source_snapshots (
            id, provider_account_id, epg_source_id, kind, status,
            checksum_sha256, byte_count, record_count, diagnostic_count, diagnostics
        )
        VALUES ($1, $2, $3, $4, 'staging', $5, $6, $7, $8, $9)
        ",
    )
    .bind(snapshot.id)
    .bind(provider_account_id)
    .bind(epg_source_id)
    .bind(snapshot.format.snapshot_kind())
    .bind(&snapshot.checksum_sha256)
    .bind(byte_count)
    .bind(record_count)
    .bind(diagnostic_count)
    .bind(&snapshot.diagnostics)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn staged_row_count(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: Uuid,
) -> Result<u64, IngestError> {
    let count: i64 = sqlx::query_scalar(
        r"
        SELECT
            (SELECT count(*) FROM provider_streams WHERE snapshot_id = $1)
          + (SELECT count(*) FROM epg_channels WHERE source_snapshot_id = $1)
          + (SELECT count(*) FROM programmes WHERE source_snapshot_id = $1)
        ",
    )
    .bind(snapshot_id)
    .fetch_one(&mut **transaction)
    .await?;
    u64::try_from(count)
        .map_err(|_| IngestError::InvalidRequest("staged snapshot count is negative"))
}

fn validate_owner(snapshot: &PreparedSnapshot) -> Result<(), IngestError> {
    match (snapshot.owner, snapshot.format.snapshot_kind()) {
        (SnapshotOwner::ProviderAccount(_), "m3u" | "xtream" | "xtream-epg")
        | (SnapshotOwner::EpgSource(_), "xmltv") => Ok(()),
        _ => Err(IngestError::InvalidRequest(
            "snapshot owner does not match source format",
        )),
    }
}

fn to_i64(value: u64) -> Result<i64, IngestError> {
    i64::try_from(value).map_err(|_| IngestError::InvalidRequest("snapshot count exceeds bigint"))
}

async fn supersede_current(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    provider_account_id: Option<Uuid>,
    epg_source_id: Option<Uuid>,
    kind: &str,
    replacement_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE source_snapshots
        SET status = 'superseded'
        WHERE provider_account_id IS NOT DISTINCT FROM $1
          AND epg_source_id IS NOT DISTINCT FROM $2
          AND kind = $3 AND status = 'active' AND id <> $4
        ",
    )
    .bind(provider_account_id)
    .bind(epg_source_id)
    .bind(kind)
    .bind(replacement_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_provider_streams(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: Uuid,
    provider_account_id: Option<Uuid>,
    streams: &StagedRows<PreparedProviderStream>,
    progress: &dyn ActivationProgress,
    records_staged: &mut u64,
    records_total: u64,
) -> Result<(), IngestError> {
    if streams.is_empty() {
        return Ok(());
    }
    let provider_account_id = provider_account_id.ok_or(IngestError::InvalidRequest(
        "provider streams require a provider owner",
    ))?;
    for chunk in streams.batches(PROVIDER_STREAM_BATCH)? {
        let chunk = chunk?;
        let mut query = QueryBuilder::<Postgres>::new(
            r"
            INSERT INTO provider_streams (
                id, snapshot_id, provider_account_id, stable_key, provider_stream_id,
                name, group_name, tvg_id, tvg_name, logo_url, channel_number,
                url_template, url_secret_ciphertext, attributes, directives, supported
            )
            ",
        );
        query.push_values(&chunk, |mut row, stream| {
            row.push_bind(stream.id)
                .push_bind(snapshot_id)
                .push_bind(provider_account_id)
                .push_bind(&stream.stable_key)
                .push_bind(&stream.provider_stream_id)
                .push_bind(&stream.name)
                .push_bind(&stream.group_name)
                .push_bind(&stream.tvg_id)
                .push_bind(&stream.tvg_name)
                .push_bind(&stream.logo_url)
                .push_bind(&stream.channel_number)
                .push_bind(&stream.endpoint.template)
                .push_bind(&stream.endpoint.secret_ciphertext)
                .push_bind(&stream.attributes)
                .push_bind(&stream.directives)
                .push_bind(stream.supported);
        });
        query.build().execute(&mut **transaction).await?;
        checkpoint_staging(progress, records_staged, chunk.len(), records_total).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_epg_channels(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: Uuid,
    provider_account_id: Option<Uuid>,
    epg_source_id: Option<Uuid>,
    snapshot_kind: &str,
    channels: &StagedRows<PreparedEpgChannel>,
    progress: &dyn ActivationProgress,
    records_staged: &mut u64,
    records_total: u64,
) -> Result<(), IngestError> {
    if channels.is_empty() {
        return Ok(());
    }
    if provider_account_id.is_some() == epg_source_id.is_some() {
        return Err(IngestError::InvalidRequest(
            "EPG channels require exactly one source owner",
        ));
    }
    // EPG channels are only valid on XMLTV and Xtream short EPG snapshots.
    // Provider-owned live catalog snapshots (m3u, xtream) must not carry
    // stray EPG channels.
    if !matches!(snapshot_kind, "xmltv" | "xtream-epg") {
        return Err(IngestError::InvalidRequest(
            "EPG channels are not valid for this snapshot kind",
        ));
    }
    for chunk in channels.batches(EPG_CHANNEL_BATCH)? {
        let chunk = chunk?;
        let mut query = QueryBuilder::<Postgres>::new(
            r"
            INSERT INTO epg_channels (
                id, source_snapshot_id, provider_account_id, epg_source_id, xmltv_id,
                display_names, icon_urls, metadata
            )
            ",
        );
        query.push_values(&chunk, |mut row, channel| {
            row.push_bind(channel.id)
                .push_bind(snapshot_id)
                .push_bind(provider_account_id)
                .push_bind(epg_source_id)
                .push_bind(&channel.xmltv_id)
                .push_bind(&channel.display_names)
                .push_bind(&channel.icon_urls)
                .push_bind(&channel.metadata);
        });
        query.build().execute(&mut **transaction).await?;
        checkpoint_staging(progress, records_staged, chunk.len(), records_total).await?;
    }
    Ok(())
}

async fn insert_programmes(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    snapshot_id: Uuid,
    programmes: &StagedRows<PreparedProgramme>,
    progress: &dyn ActivationProgress,
    records_staged: &mut u64,
    records_total: u64,
) -> Result<(), IngestError> {
    for chunk in programmes.batches(PROGRAMME_BATCH)? {
        let chunk = chunk?;
        let mut query = QueryBuilder::<Postgres>::new(
            r"
            INSERT INTO programmes (
                id, source_snapshot_id, epg_channel_id, starts_at, stops_at,
                original_start, original_stop, title, subtitle, description,
                categories, metadata
            )
            ",
        );
        query.push_values(&chunk, |mut row, programme| {
            row.push_bind(programme.id)
                .push_bind(snapshot_id)
                .push_bind(programme.epg_channel_id)
                .push_bind(programme.starts_at)
                .push_bind(programme.stops_at)
                .push_bind(&programme.original_start)
                .push_bind(&programme.original_stop)
                .push_bind(&programme.title)
                .push_bind(&programme.subtitle)
                .push_bind(&programme.description)
                .push_bind(&programme.categories)
                .push_bind(&programme.metadata);
        });
        query.build().execute(&mut **transaction).await?;
        checkpoint_staging(progress, records_staged, chunk.len(), records_total).await?;
    }
    Ok(())
}

async fn checkpoint_staging(
    progress: &dyn ActivationProgress,
    records_staged: &mut u64,
    batch_len: usize,
    records_total: u64,
) -> Result<(), IngestError> {
    let batch_len = u64::try_from(batch_len)
        .map_err(|_| IngestError::InvalidRequest("snapshot batch count exceeds u64"))?;
    *records_staged = records_staged
        .checked_add(batch_len)
        .ok_or(IngestError::InvalidRequest(
            "snapshot staging count exceeds u64",
        ))?;
    if *records_staged > records_total {
        return Err(IngestError::InvalidRequest(
            "snapshot staging count exceeds record count",
        ));
    }
    progress.checkpoint(*records_staged, records_total).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IngestFormat, ProtectedEndpoint};
    use serde_json::json;

    fn snapshot(owner: SnapshotOwner, format: crate::IngestFormat) -> PreparedSnapshot {
        PreparedSnapshot {
            id: Uuid::now_v7(),
            owner,
            format,
            checksum_sha256: "00".repeat(32),
            byte_count: 1,
            record_count: 1,
            diagnostic_count: 0,
            diagnostics: json!([]),
            provider_streams: StagedRows::new(1_000),
            epg_channels: StagedRows::new(1_000),
            programmes: StagedRows::new(1_000),
        }
    }

    #[test]
    fn owner_validation_is_fail_closed() {
        let id = Uuid::now_v7();
        assert!(
            validate_owner(&snapshot(
                SnapshotOwner::ProviderAccount(id),
                IngestFormat::M3u
            ))
            .is_ok()
        );
        assert!(
            validate_owner(&snapshot(SnapshotOwner::EpgSource(id), IngestFormat::Xmltv)).is_ok()
        );
        assert!(
            validate_owner(&snapshot(SnapshotOwner::EpgSource(id), IngestFormat::M3u)).is_err()
        );
        assert!(
            validate_owner(&snapshot(
                SnapshotOwner::ProviderAccount(id),
                IngestFormat::Xmltv
            ))
            .is_err()
        );
    }

    #[test]
    fn bigint_conversion_rejects_overflow() {
        assert_eq!(to_i64(7).expect("small"), 7);
        assert!(to_i64(u64::MAX).is_err());
    }

    #[test]
    fn endpoint_debug_does_not_expose_persisted_ciphertext() {
        let stream = PreparedProviderStream {
            id: Uuid::now_v7(),
            stable_key: "one".into(),
            provider_stream_id: None,
            name: "One".into(),
            group_name: None,
            tvg_id: None,
            tvg_name: None,
            logo_url: None,
            channel_number: None,
            endpoint: ProtectedEndpoint {
                template: "https://example.test/{secret}".into(),
                secret_ciphertext: Some(b"plaintext-password".to_vec()),
            },
            attributes: json!({}),
            directives: json!([]),
            supported: true,
        };
        assert!(!format!("{stream:?}").contains("plaintext-password"));
    }
}
