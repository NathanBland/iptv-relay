//! `PostgreSQL` persistence and durable job coordination.

mod catalog;

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

pub use catalog::{
    CatalogRepository, ChannelPage, ChannelQuery, ChannelRow, EpgMappingStats, ProgrammePage,
    ProgrammeQuery, ProgrammeRow, ReconcileStats, SystemCounts,
};

/// Embedded database migrations for the service schema.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("job {job_id} is not owned by worker {worker_id}")]
    JobOwnership { job_id: Uuid, worker_id: String },
    #[error("IPTV_MASTER_KEY must be valid base64 encoding exactly 32 bytes")]
    InvalidMasterKey,
    #[error("source configuration encryption failed")]
    Encryption,
    #[error("source configuration decryption failed")]
    Decryption,
    #[error("invalid source configuration: {0}")]
    InvalidSource(String),
    #[error("source {0} was not found")]
    SourceNotFound(Uuid),
    #[error("source name already exists")]
    SourceConflict,
    #[error("job {0} was not found")]
    JobNotFound(Uuid),
}

const ENCRYPTED_VALUE_VERSION: u8 = 1;
const XNONCE_LENGTH: usize = 24;

#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl fmt::Debug for MasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MasterKey")
            .field(&"<redacted>")
            .finish()
    }
}

impl MasterKey {
    /// Parses a standard-base64 encoded 256-bit master key.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::InvalidMasterKey`] without echoing the
    /// supplied value when it is malformed or does not decode to 32 bytes.
    pub fn from_base64(encoded: &str) -> Result<Self, PersistenceError> {
        let bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|_| PersistenceError::InvalidMasterKey)?;
        let key = bytes
            .try_into()
            .map_err(|_| PersistenceError::InvalidMasterKey)?;
        Ok(Self(key))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn encrypt_secret(
        &self,
        plaintext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, PersistenceError> {
        SourceCipher::new(self.clone()).encrypt(plaintext, associated_data)
    }
}

#[derive(Clone)]
struct SourceCipher {
    key: MasterKey,
}

impl fmt::Debug for SourceCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceCipher")
            .field("key", &"<redacted>")
            .finish()
    }
}

impl SourceCipher {
    fn new(key: MasterKey) -> Self {
        Self { key }
    }

    fn encrypt(
        &self,
        plaintext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, PersistenceError> {
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&self.key.0));
        let mut nonce = [0_u8; XNONCE_LENGTH];
        getrandom::fill(&mut nonce).map_err(|_| PersistenceError::Encryption)?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: associated_data,
                },
            )
            .map_err(|_| PersistenceError::Encryption)?;
        let mut envelope = Vec::with_capacity(1 + nonce.len() + ciphertext.len());
        envelope.push(ENCRYPTED_VALUE_VERSION);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ciphertext);
        Ok(envelope)
    }

    fn decrypt(
        &self,
        envelope: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, PersistenceError> {
        if envelope.first() != Some(&ENCRYPTED_VALUE_VERSION) || envelope.len() <= 1 + XNONCE_LENGTH
        {
            return Err(PersistenceError::Decryption);
        }
        let nonce = XNonce::from_slice(&envelope[1..=XNONCE_LENGTH]);
        XChaCha20Poly1305::new(Key::from_slice(&self.key.0))
            .decrypt(
                nonce,
                Payload {
                    msg: &envelope[1 + XNONCE_LENGTH..],
                    aad: associated_data,
                },
            )
            .map_err(|_| PersistenceError::Decryption)
    }
}

#[derive(Clone, Debug)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Opens a bounded `PostgreSQL` connection pool.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] if the initial database
    /// connection cannot be established.
    pub async fn connect(
        database_url: &str,
        max_connections: u32,
    ) -> Result<Self, PersistenceError> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(max_connections)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies all embedded schema migrations.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Migration`] when a migration cannot be
    /// applied atomically.
    pub async fn migrate(&self) -> Result<(), PersistenceError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Checks that the pool can execute a trivial query.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    pub async fn health(&self) -> Result<(), PersistenceError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SourceKind {
    M3u,
    Xtream,
    Xmltv,
    NetworkTuner,
}

impl SourceKind {
    /// Parses the stable source-kind spelling used by the control API.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::InvalidSource`] for an unknown kind.
    pub fn parse_api(value: &str) -> Result<Self, PersistenceError> {
        match value {
            "M3U" => Ok(Self::M3u),
            "Xtream" => Ok(Self::Xtream),
            "XMLTV" => Ok(Self::Xmltv),
            "Network tuner" => Ok(Self::NetworkTuner),
            _ => Err(PersistenceError::InvalidSource(
                "kind must be M3U, Xtream, XMLTV, or Network tuner".to_owned(),
            )),
        }
    }

    pub fn api_name(self) -> &'static str {
        match self {
            Self::M3u => "M3U",
            Self::Xtream => "Xtream",
            Self::Xmltv => "XMLTV",
            Self::NetworkTuner => "Network tuner",
        }
    }

    pub fn database_name(self) -> &'static str {
        match self {
            Self::M3u => "m3u",
            Self::Xtream => "xtream",
            Self::Xmltv => "xmltv",
            Self::NetworkTuner => "network-tuner",
        }
    }

    fn from_database(value: &str) -> Result<Self, PersistenceError> {
        match value {
            "m3u" => Ok(Self::M3u),
            "xtream" => Ok(Self::Xtream),
            "xmltv" => Ok(Self::Xmltv),
            "network-tuner" => Ok(Self::NetworkTuner),
            _ => Err(PersistenceError::InvalidSource(
                "stored source kind is unsupported".to_owned(),
            )),
        }
    }
}

#[derive(Clone)]
pub struct NewSource {
    pub name: String,
    pub kind: SourceKind,
    pub endpoint: String,
}

impl fmt::Debug for NewSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewSource")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("endpoint", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: SourceKind,
    pub state: String,
    pub channels: usize,
    pub last_sync: DateTime<Utc>,
    pub endpoint: String,
    pub revision: i64,
}

#[derive(Clone, PartialEq)]
pub struct DecryptedSource {
    pub id: Uuid,
    pub name: String,
    pub kind: SourceKind,
    pub endpoint: String,
    pub timezone: String,
    pub revision: i64,
}

impl fmt::Debug for DecryptedSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecryptedSource")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("endpoint", &"<redacted>")
            .field("timezone", &self.timezone)
            .field("revision", &self.revision)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct CreatedSource {
    pub source: SourceSummary,
    pub refresh_job: JobRecord,
}

#[derive(Debug, FromRow)]
struct SourceSummaryRow {
    id: Uuid,
    name: String,
    kind: String,
    endpoint: String,
    revision: i64,
    updated_at: DateTime<Utc>,
    activated_at: Option<DateTime<Utc>>,
    record_count: i64,
    job_status: Option<String>,
}

#[derive(Debug, FromRow)]
struct EncryptedSourceRow {
    id: Uuid,
    name: String,
    kind: String,
    ciphertext: Vec<u8>,
    timezone: String,
    revision: i64,
}

#[derive(Clone, Debug)]
pub struct SourceRepository {
    pool: PgPool,
    cipher: SourceCipher,
}

impl SourceRepository {
    pub fn new(pool: PgPool, master_key: MasterKey) -> Self {
        Self {
            pool,
            cipher: SourceCipher::new(master_key),
        }
    }

    /// Lists source metadata without decrypting credential-bearing endpoints.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError`] if `PostgreSQL` data is unavailable or has
    /// an unsupported stored source kind.
    pub async fn list(&self) -> Result<Vec<SourceSummary>, PersistenceError> {
        let rows = sqlx::query_as::<_, SourceSummaryRow>(
            r"
            WITH source_rows AS (
                SELECT id, name, source_type AS kind, base_url_template AS endpoint,
                       revision, updated_at
                FROM provider_accounts
                WHERE enabled = true
                UNION ALL
                SELECT id, name, 'xmltv' AS kind, url_template AS endpoint,
                       revision, updated_at
                FROM epg_sources
                WHERE enabled = true
            )
            SELECT sources.id, sources.name, sources.kind, sources.endpoint,
                   sources.revision, sources.updated_at,
                   snapshot.activated_at,
                   COALESCE(snapshot.record_count, 0) AS record_count,
                   latest_job.status AS job_status
            FROM source_rows AS sources
            LEFT JOIN LATERAL (
                SELECT activated_at, record_count
                FROM source_snapshots
                WHERE status = 'active'
                  AND (provider_account_id = sources.id OR epg_source_id = sources.id)
                ORDER BY activated_at DESC NULLS LAST
                LIMIT 1
            ) AS snapshot ON true
            LEFT JOIN LATERAL (
                SELECT status
                FROM jobs
                WHERE kind = 'refresh-source'
                  AND payload->>'sourceId' = sources.id::text
                ORDER BY created_at DESC
                LIMIT 1
            ) AS latest_job ON true
            ORDER BY lower(sources.name), sources.id
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(source_summary_from_row).collect()
    }

    /// Creates a durable encrypted source, refresh job, and audit event in one
    /// `PostgreSQL` transaction.
    ///
    /// # Errors
    ///
    /// Returns validation, encryption, conflict, or database errors. The
    /// transaction is rolled back if any insert fails.
    #[allow(clippy::too_many_lines)]
    pub async fn create(
        &self,
        source: &NewSource,
        actor: &str,
    ) -> Result<CreatedSource, PersistenceError> {
        validate_source(source)?;
        let safe_endpoint = redact_source_endpoint(&source.endpoint)?;
        let source_id = Uuid::now_v7();
        let associated_data = source_associated_data(source_id, source.kind);
        let ciphertext = self
            .cipher
            .encrypt(source.endpoint.as_bytes(), associated_data.as_bytes())?;
        let now = Utc::now();
        let job_id = Uuid::now_v7();
        let correlation_id = Uuid::now_v7();
        let mut transaction = self.pool.begin().await?;

        let duplicate: bool = sqlx::query_scalar(
            r"
            SELECT EXISTS (
                SELECT 1 FROM provider_accounts WHERE lower(name) = lower($1)
                UNION ALL
                SELECT 1 FROM epg_sources WHERE lower(name) = lower($1)
            )
            ",
        )
        .bind(source.name.trim())
        .fetch_one(&mut *transaction)
        .await?;
        if duplicate {
            return Err(PersistenceError::SourceConflict);
        }

        match source.kind {
            SourceKind::Xmltv => {
                sqlx::query(
                    r"
                    INSERT INTO epg_sources
                        (id, name, url_template, secret_ciphertext, updated_at)
                    VALUES ($1, $2, $3, $4, $5)
                    ",
                )
                .bind(source_id)
                .bind(source.name.trim())
                .bind(&safe_endpoint)
                .bind(&ciphertext)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            }
            SourceKind::M3u | SourceKind::Xtream | SourceKind::NetworkTuner => {
                sqlx::query(
                    r"
                    INSERT INTO provider_accounts
                        (id, name, source_type, base_url_template, secret_ciphertext, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6)
                    ",
                )
                .bind(source_id)
                .bind(source.name.trim())
                .bind(source.kind.database_name())
                .bind(&safe_endpoint)
                .bind(&ciphertext)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            }
        }

        let payload = serde_json::json!({
            "sourceId": source_id,
            "sourceType": source.kind.database_name(),
        });
        let refresh_job = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, payload, available_at)
            VALUES ($1, 'refresh-source', $2, $3)
            RETURNING *
            ",
        )
        .bind(job_id)
        .bind(&payload)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            r"
            INSERT INTO audit_events
                (id, actor, action, resource_type, resource_id, correlation_id, details)
            VALUES ($1, $2, 'source.create', 'source', $3, $4, $5)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(actor)
        .bind(source_id)
        .bind(correlation_id)
        .bind(serde_json::json!({
            "kind": source.kind.database_name(),
            "name": source.name.trim(),
            "refreshJobId": job_id,
        }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(CreatedSource {
            source: SourceSummary {
                id: source_id,
                name: source.name.trim().to_owned(),
                kind: source.kind,
                state: "syncing".to_owned(),
                channels: 0,
                last_sync: now,
                endpoint: safe_endpoint,
                revision: 1,
            },
            refresh_job,
        })
    }

    /// Decrypts a source for a trusted background job. No decrypted value is
    /// included in `Debug` output from the repository or persisted job payload.
    ///
    /// # Errors
    ///
    /// Returns not-found, database, or authenticated-decryption errors.
    pub async fn load_for_job(&self, source_id: Uuid) -> Result<DecryptedSource, PersistenceError> {
        let row = sqlx::query_as::<_, EncryptedSourceRow>(
            r"
            SELECT id, name, source_type AS kind,
                   secret_ciphertext AS ciphertext, source_timezone AS timezone, revision
            FROM provider_accounts
            WHERE id = $1 AND enabled = true AND secret_ciphertext IS NOT NULL
            UNION ALL
            SELECT id, name, 'xmltv' AS kind,
                   secret_ciphertext AS ciphertext, timezone, revision
            FROM epg_sources
            WHERE id = $1 AND enabled = true AND secret_ciphertext IS NOT NULL
            LIMIT 1
            ",
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(PersistenceError::SourceNotFound(source_id))?;
        let kind = SourceKind::from_database(&row.kind)?;
        let associated_data = source_associated_data(row.id, kind);
        let endpoint = self
            .cipher
            .decrypt(&row.ciphertext, associated_data.as_bytes())?;
        let endpoint = String::from_utf8(endpoint).map_err(|_| PersistenceError::Decryption)?;
        Ok(DecryptedSource {
            id: row.id,
            name: row.name,
            kind,
            endpoint,
            timezone: row.timezone,
            revision: row.revision,
        })
    }
}

fn validate_source(source: &NewSource) -> Result<(), PersistenceError> {
    let name_length = source.name.trim().chars().count();
    if !(2..=128).contains(&name_length) {
        return Err(PersistenceError::InvalidSource(
            "name must contain 2-128 characters".to_owned(),
        ));
    }
    if source.endpoint.len() > 8_192 {
        return Err(PersistenceError::InvalidSource(
            "endpoint must not exceed 8192 bytes".to_owned(),
        ));
    }
    redact_source_endpoint(&source.endpoint).map(|_| ())
}

fn source_associated_data(source_id: Uuid, kind: SourceKind) -> String {
    format!("iptv-source:v1:{source_id}:{}", kind.database_name())
}

fn redact_source_endpoint(endpoint: &str) -> Result<String, PersistenceError> {
    let mut parsed = url::Url::parse(endpoint).map_err(|_| {
        PersistenceError::InvalidSource("endpoint must be an absolute HTTP(S) URL".to_owned())
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(PersistenceError::InvalidSource(
            "endpoint must be an absolute HTTP(S) URL".to_owned(),
        ));
    }
    parsed.set_username("").map_err(|()| {
        PersistenceError::InvalidSource("endpoint user info is invalid".to_owned())
    })?;
    parsed.set_password(None).map_err(|()| {
        PersistenceError::InvalidSource("endpoint user info is invalid".to_owned())
    })?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    if parsed.path() != "/" && !parsed.path().is_empty() {
        parsed.set_path("/…");
    }
    Ok(parsed.to_string())
}

fn source_summary_from_row(row: SourceSummaryRow) -> Result<SourceSummary, PersistenceError> {
    let kind = SourceKind::from_database(&row.kind)?;
    let state = match row.job_status.as_deref() {
        Some("queued" | "running") => "syncing",
        Some("failed") => "degraded",
        Some("cancelled") => "offline",
        _ => "healthy",
    };
    Ok(SourceSummary {
        id: row.id,
        name: row.name,
        kind,
        state: state.to_owned(),
        channels: usize::try_from(row.record_count.max(0)).unwrap_or(usize::MAX),
        last_sync: row.activated_at.unwrap_or(row.updated_at),
        endpoint: row.endpoint,
        revision: row.revision,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, FromRow, PartialEq)]
pub struct JobRecord {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub priority: i32,
    pub payload: Value,
    pub progress: Value,
    pub attempts: i32,
    pub max_attempts: i32,
    pub available_at: DateTime<Utc>,
    pub locked_by: Option<String>,
    pub locked_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewJob {
    pub kind: String,
    pub priority: i32,
    pub payload: Value,
    pub max_attempts: i32,
    pub available_at: DateTime<Utc>,
}

impl NewJob {
    pub fn immediate(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            priority: 0,
            payload,
            max_attempts: 3,
            available_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct JobRepository {
    pool: PgPool,
}

impl JobRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persists a new queued job.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert fails.
    pub async fn enqueue(&self, job: &NewJob) -> Result<JobRecord, PersistenceError> {
        let id = Uuid::now_v7();
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, priority, payload, max_attempts, available_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            ",
        )
        .bind(id)
        .bind(&job.kind)
        .bind(job.priority)
        .bind(&job.payload)
        .bind(job.max_attempts)
        .bind(job.available_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(record)
    }

    /// Lists recent jobs for control-plane status views.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    pub async fn list_recent(&self, limit: u16) -> Result<Vec<JobRecord>, PersistenceError> {
        let limit = i64::from(limit.clamp(1, 500));
        let records = sqlx::query_as::<_, JobRecord>(
            r"
            SELECT *
            FROM jobs
            ORDER BY created_at DESC, id DESC
            LIMIT $1
            ",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(records)
    }

    /// Cancels a queued or running job. Workers observe cancellation through
    /// [`Self::is_cancelled`] or a failed ownership-checked heartbeat.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobNotFound`] if no job exists and a
    /// database error if cancellation cannot be persisted.
    pub async fn cancel(&self, job_id: Uuid) -> Result<bool, PersistenceError> {
        let status: Option<String> = sqlx::query_scalar(
            r"
            UPDATE jobs
            SET status = 'cancelled', completed_at = now(), updated_at = now(),
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL
            WHERE id = $1 AND status IN ('queued', 'running')
            RETURNING status
            ",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        if status.is_some() {
            return Ok(true);
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM jobs WHERE id = $1)")
            .bind(job_id)
            .fetch_one(&self.pool)
            .await?;
        if exists {
            Ok(false)
        } else {
            Err(PersistenceError::JobNotFound(job_id))
        }
    }

    /// Reports whether a worker should stop processing a cancelled job.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobNotFound`] for an unknown job and a
    /// database error if status cannot be read.
    pub async fn is_cancelled(&self, job_id: Uuid) -> Result<bool, PersistenceError> {
        let status: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await?;
        status
            .map(|status| status == "cancelled")
            .ok_or(PersistenceError::JobNotFound(job_id))
    }

    /// Atomically claims the next available job without blocking other workers.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the claim query fails.
    pub async fn claim(&self, worker_id: &str) -> Result<Option<JobRecord>, PersistenceError> {
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            WITH candidate AS (
                SELECT id
                FROM jobs
                WHERE status = 'queued'
                  AND available_at <= now()
                  AND attempts < max_attempts
                ORDER BY priority DESC, created_at ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE jobs
            SET status = 'running',
                locked_by = $1,
                locked_at = now(),
                heartbeat_at = now(),
                attempts = attempts + 1,
                updated_at = now()
            FROM candidate
            WHERE jobs.id = candidate.id
            RETURNING jobs.*
            ",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(record)
    }

    /// Updates progress for a job owned by the given worker.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobOwnership`] for a stale owner and
    /// [`PersistenceError::Database`] when the update fails.
    pub async fn heartbeat(
        &self,
        job_id: Uuid,
        worker_id: &str,
        progress: &Value,
    ) -> Result<(), PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET heartbeat_at = now(), progress = $3, updated_at = now()
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(progress)
        .execute(&self.pool)
        .await?;
        ensure_owned(result.rows_affected(), job_id, worker_id)
    }

    /// Marks an owned job as successfully completed.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobOwnership`] for a stale owner and
    /// [`PersistenceError::Database`] when the update fails.
    pub async fn succeed(&self, job_id: Uuid, worker_id: &str) -> Result<(), PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET status = 'succeeded', completed_at = now(), updated_at = now(),
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job_id)
        .bind(worker_id)
        .execute(&self.pool)
        .await?;
        ensure_owned(result.rows_affected(), job_id, worker_id)
    }

    /// Records a redacted job failure and either retries or finalizes it.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobOwnership`] for a stale owner and
    /// [`PersistenceError::Database`] when the update fails.
    pub async fn fail(
        &self,
        job: &JobRecord,
        worker_id: &str,
        error: &str,
        retry_at: DateTime<Utc>,
    ) -> Result<(), PersistenceError> {
        let retry = job.attempts < job.max_attempts;
        let status = if retry { "queued" } else { "failed" };
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET status = $3, last_error = $4, available_at = $5,
                completed_at = CASE WHEN $3 = 'failed' THEN now() ELSE NULL END,
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL, updated_at = now()
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job.id)
        .bind(worker_id)
        .bind(status)
        .bind(redact_error(error))
        .bind(retry_at)
        .execute(&self.pool)
        .await?;
        ensure_owned(result.rows_affected(), job.id, worker_id)
    }
}

fn ensure_owned(rows: u64, job_id: Uuid, worker_id: &str) -> Result<(), PersistenceError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(PersistenceError::JobOwnership {
            job_id,
            worker_id: worker_id.to_owned(),
        })
    }
}

/// Prevents common URL credentials from entering a persisted job error.
pub fn redact_error(error: &str) -> String {
    let mut redacted = error.chars().take(2_048).collect::<String>();
    for scheme in ["https://", "http://"] {
        while let Some(start) = redacted.to_ascii_lowercase().find(scheme) {
            let end = redacted[start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '\'' | '"')
                })
                .map_or(redacted.len(), |offset| start + offset);
            redacted.replace_range(start..end, "[REDACTED_URL]");
        }
    }
    for key in ["username", "password", "token", "auth", "secret", "key"] {
        let mut search_from = 0;
        while let Some(relative) = redacted[search_from..]
            .to_ascii_lowercase()
            .find(&format!("{key}="))
        {
            let start = search_from + relative + key.len() + 1;
            let end = redacted[start..]
                .find(['&', ' ', '\'', '"'])
                .map_or(redacted.len(), |offset| start + offset);
            redacted.replace_range(start..end, "[REDACTED]");
            search_from = start + "[REDACTED]".len();
        }
    }
    redacted
}

/// Recursively redacts job diagnostics before they cross the control API.
pub fn redact_diagnostics(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let sensitive = [
                        "username", "password", "token", "auth", "secret", "key", "url", "endpoint",
                    ]
                    .iter()
                    .any(|candidate| normalized.contains(candidate));
                    (
                        key.clone(),
                        if sensitive {
                            Value::String("[REDACTED]".to_owned())
                        } else {
                            redact_diagnostics(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_diagnostics).collect()),
        Value::String(value) => Value::String(redact_error(value)),
        scalar => scalar.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(byte: u8) -> MasterKey {
        MasterKey::from_bytes([byte; 32])
    }

    #[test]
    fn master_key_requires_exactly_32_base64_bytes_and_redacts_debug() {
        let encoded = STANDARD.encode([7_u8; 32]);
        let key = MasterKey::from_base64(&encoded).unwrap();
        let debug = format!("{key:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&encoded));
        for invalid in [
            "",
            "not base64!",
            &STANDARD.encode([1_u8; 31]),
            &STANDARD.encode([1_u8; 33]),
        ] {
            assert!(matches!(
                MasterKey::from_base64(invalid),
                Err(PersistenceError::InvalidMasterKey)
            ));
        }
    }

    #[test]
    fn source_encryption_uses_unique_nonces_and_detects_tampering() {
        let key = test_key(1);
        let cipher = SourceCipher::new(key.clone());
        let plaintext = b"https://user:password@provider.test/live/secret?token=value";
        let associated_data = b"iptv-source:v1:source:m3u";
        let first = key.encrypt_secret(plaintext, associated_data).unwrap();
        let second = cipher.encrypt(plaintext, associated_data).unwrap();
        assert_ne!(first, second);
        assert_eq!(cipher.decrypt(&first, associated_data).unwrap(), plaintext);
        assert!(
            SourceCipher::new(test_key(2))
                .decrypt(&first, associated_data)
                .is_err()
        );
        assert!(cipher.decrypt(&first, b"different-source").is_err());
        let mut tampered = first;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(cipher.decrypt(&tampered, associated_data).is_err());
        assert!(cipher.decrypt(&[], associated_data).is_err());
    }

    #[test]
    fn source_validation_and_display_endpoint_never_expose_credentials() {
        let endpoint = "https://alice:password@provider.test:8443/live/alice/secret/list.m3u?token=value#fragment";
        let redacted = redact_source_endpoint(endpoint).unwrap();
        assert_eq!(redacted, "https://provider.test:8443/%E2%80%A6");
        for secret in ["alice", "password", "secret", "token", "value"] {
            assert!(!redacted.contains(secret));
        }
        assert!(redact_source_endpoint("file:///private/source").is_err());
        assert!(redact_source_endpoint("relative/path").is_err());
        assert!(
            validate_source(&NewSource {
                name: "x".to_owned(),
                kind: SourceKind::M3u,
                endpoint: "https://provider.test/list.m3u".to_owned(),
            })
            .is_err()
        );
        assert!(
            validate_source(&NewSource {
                name: "Valid".to_owned(),
                kind: SourceKind::M3u,
                endpoint: format!("https://provider.test/{}", "x".repeat(8_192)),
            })
            .is_err()
        );
        let source = NewSource {
            name: "Private provider".to_owned(),
            kind: SourceKind::M3u,
            endpoint: endpoint.to_owned(),
        };
        let debug = format!("{source:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("password"));
        let decrypted = DecryptedSource {
            id: Uuid::nil(),
            name: source.name,
            kind: source.kind,
            endpoint: source.endpoint,
            timezone: "UTC".to_owned(),
            revision: 1,
        };
        assert!(!format!("{decrypted:?}").contains("password"));
    }

    #[test]
    fn source_kinds_round_trip_between_api_and_database_names() {
        for (api, database, expected) in [
            ("M3U", "m3u", SourceKind::M3u),
            ("Xtream", "xtream", SourceKind::Xtream),
            ("XMLTV", "xmltv", SourceKind::Xmltv),
            ("Network tuner", "network-tuner", SourceKind::NetworkTuner),
        ] {
            assert_eq!(SourceKind::parse_api(api).unwrap(), expected);
            assert_eq!(expected.api_name(), api);
            assert_eq!(expected.database_name(), database);
            assert_eq!(SourceKind::from_database(database).unwrap(), expected);
        }
        assert!(SourceKind::parse_api("other").is_err());
        assert!(SourceKind::from_database("other").is_err());
    }

    #[test]
    fn immediate_job_has_safe_defaults() {
        let job = NewJob::immediate("refresh-m3u", serde_json::json!({"source": "one"}));
        assert_eq!(job.kind, "refresh-m3u");
        assert_eq!(job.priority, 0);
        assert_eq!(job.max_attempts, 3);
    }

    #[test]
    fn redacts_query_credentials() {
        let value = redact_error(
            "fetch http://provider/get?username=alice&password=secret&token=abc failed",
        );
        assert!(!value.contains("alice"));
        assert!(!value.contains("secret"));
        assert!(!value.contains("abc"));
        assert!(value.contains("[REDACTED_URL]"));
        assert!(!value.contains("provider"));
        assert_eq!(redact_error(&"x".repeat(3_000)).len(), 2_048);
    }

    #[test]
    fn recursively_redacts_job_diagnostics() {
        let value = redact_diagnostics(&serde_json::json!({
            "processed": 4,
            "nested": [{"endpointUrl": "https://secret"}],
            "message": "request?username=alice&password=secret failed",
            "apiToken": "value"
        }));
        let rendered = value.to_string();
        for secret in ["https://secret", "alice", "password=secret", "value"] {
            assert!(!rendered.contains(secret));
        }
        assert_eq!(value["processed"], 4);
    }

    #[test]
    fn ownership_guard_rejects_missing_update() {
        let job_id = Uuid::nil();
        let error = ensure_owned(0, job_id, "worker-a").unwrap_err();
        assert!(matches!(error, PersistenceError::JobOwnership { .. }));
    }
}
