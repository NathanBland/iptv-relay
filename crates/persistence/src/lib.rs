//! `PostgreSQL` persistence and durable job coordination.

mod catalog;

use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

pub use catalog::{
    CatalogRepository, ChannelAliasRow, ChannelAliasStats, ChannelPage,
    ChannelPlaybackCandidateRow, ChannelPlaybackPlan, ChannelQuery, ChannelRow,
    ChannelStreamCandidateRow, ChannelStreamSourceRow, CreateChannelAliasInput,
    CreateEventTemplate, CreateRecordingInput, CreateRecordingRuleInput, CreateStreamProfileInput,
    CreateUserInput, DEFAULT_RECONCILIATION_BATCH_SIZE, ENVIRONMENT_OUTPUT_PROFILE_ID,
    ENVIRONMENT_OUTPUT_PROFILE_NAME, EpgChannelSearchRow, EpgMappingPage, EpgMappingRow,
    EpgMappingStats, EventChannelRow, EventTemplateQuery, EventTemplateRow,
    EventTemplateSuggestion, EventTemplateUpdate, LineupApplyStats, LineupCategoryRow,
    LineupChannelRow, LineupTemplateRow, OperatorSettingOverrideRow, OperatorSettingOverrides,
    OperatorSettingRevisionRow, OperatorSettingScopeState, OutputProfileRow,
    OutputProfileTokenHash, ProgrammePage, ProgrammeQuery, ProgrammeRow,
    ProviderReconciliationFinalization, ProviderReconciliationPartitionResult,
    ProviderReconciliationPhase, ProviderReconciliationProgress, ProviderReconciliationRun,
    ProviderReconciliationRunProgress, ProviderReconciliationUpdate, ReconcileStats,
    ReconciliationRevisionRow, ReconciliationRollbackStats, RecordingRow, RecordingRuleRow,
    RecordingStats, RegionFilterStats, RegionPrefixRow, RegionSettingsRow, ReviewCandidateRow,
    StreamHealthPage, StreamHealthRow, StreamHealthStats, StreamHealthUpdate, StreamProbeTargetRow,
    StreamProfileRow, SystemCounts, UnmappedChannelPage, UnmappedChannelRow, UpdateUserInput,
    UserRow,
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
    #[error("output profile tuner count must be at least one")]
    InvalidOutputProfileTunerCount,
    #[error("operator setting revision conflict: expected {expected}, found {actual}")]
    SettingRevisionConflict { expected: i64, actual: i64 },
    #[error("operator setting revision {target} was not found for {scope}:{scope_id}")]
    SettingRevisionNotFound {
        scope: String,
        scope_id: String,
        target: i64,
    },
    #[error("reconciliation revision {target} was not found for account {account_id}")]
    ReconciliationRevisionNotFound { account_id: Uuid, target: i64 },
    #[error("operator API token {0} was not found")]
    OperatorApiTokenNotFound(Uuid),
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

    /// Decrypts a secret previously encrypted with `encrypt_secret`.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Decryption`] when the ciphertext is invalid
    /// or the associated data does not match.
    #[allow(clippy::missing_errors_doc)]
    pub fn decrypt_secret(
        &self,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, PersistenceError> {
        SourceCipher::new(self.clone()).decrypt(ciphertext, associated_data)
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
        // Create the shared extension once before schema migrations. PostgreSQL
        // extension creation can race when each API test uses a new schema.
        let mut connection = self.pool.acquire().await?;
        let lock_query = sqlx::query("SELECT pg_advisory_lock(7283912041)");
        lock_query.execute(&mut *connection).await?;
        sqlx::query("CREATE EXTENSION IF NOT EXISTS btree_gist WITH SCHEMA public")
            .execute(&mut *connection)
            .await?;
        let unlock_query = sqlx::query("SELECT pg_advisory_unlock(7283912041)");
        unlock_query.execute(&mut *connection).await?;
        drop(connection);
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

    /// Get the durable bootstrap bearer authorization state.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] if the state cannot load.
    pub async fn bootstrap_bearer_enabled(&self) -> Result<bool, PersistenceError> {
        sqlx::query_scalar(
            "SELECT bootstrap_bearer_enabled FROM authentication_state WHERE id = true",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(PersistenceError::from)
    }

    /// Disable bootstrap bearer authorization in durable state.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] if the state cannot update.
    pub async fn disable_bootstrap_bearer(&self) -> Result<(), PersistenceError> {
        sqlx::query(
            "UPDATE authentication_state \
             SET bootstrap_bearer_enabled = false, updated_at = now() \
             WHERE id = true AND bootstrap_bearer_enabled",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Lists operator API tokens without exposing their plaintext values.
    ///
    /// The returned rows contain only the stored hash and token metadata.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_operator_api_tokens(
        &self,
    ) -> Result<Vec<OperatorApiTokenRow>, PersistenceError> {
        let rows = sqlx::query_as::<_, OperatorApiTokenRow>(
            r"
            SELECT id, name, token_hash, scopes, expires_at, revoked_at,
                   created_by, created_at, last_used_at
            FROM operator_api_tokens
            ORDER BY created_at DESC, id DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Creates an operator API token and records its audit event atomically.
    ///
    /// Only the hash is stored. The caller must retain the generated plaintext
    /// token and display it once to the operator.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert or audit event fails.
    #[allow(clippy::too_many_arguments, clippy::missing_errors_doc)]
    pub async fn create_operator_api_token(
        &self,
        id: Uuid,
        name: &str,
        token_hash: &[u8; 32],
        scopes: &[String],
        expires_at: Option<DateTime<Utc>>,
        actor: &str,
    ) -> Result<OperatorApiTokenRow, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query_as::<_, OperatorApiTokenRow>(
            r"
            INSERT INTO operator_api_tokens
                (id, name, token_hash, scopes, expires_at, created_by)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, name, token_hash, scopes, expires_at, revoked_at,
                      created_by, created_at, last_used_at
            ",
        )
        .bind(id)
        .bind(name)
        .bind(token_hash.as_slice())
        .bind(scopes)
        .bind(expires_at)
        .bind(actor)
        .fetch_one(&mut *transaction)
        .await?;
        self.insert_operator_token_audit(
            &mut transaction,
            actor,
            "operator_token.create",
            id,
            serde_json::json!({
                "name": name,
                "scopes": scopes,
                "expiresAt": expires_at,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok(row)
    }

    /// Rotates an active operator API token and records both lifecycle events.
    ///
    /// Rotation revokes the old token before it inserts the replacement. Only
    /// the replacement hash is stored and its plaintext is returned to the caller.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the transaction fails.
    #[allow(clippy::too_many_arguments, clippy::missing_errors_doc)]
    pub async fn rotate_operator_api_token(
        &self,
        old_id: Uuid,
        new_id: Uuid,
        name: &str,
        token_hash: &[u8; 32],
        scopes: &[String],
        expires_at: Option<DateTime<Utc>>,
        actor: &str,
    ) -> Result<Option<OperatorApiTokenRow>, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let old_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM operator_api_tokens WHERE id = $1 AND revoked_at IS NULL)",
        )
        .bind(old_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !old_exists {
            transaction.rollback().await?;
            return Ok(None);
        }
        let revoked = sqlx::query(
            "UPDATE operator_api_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(old_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if revoked != 1 {
            transaction.rollback().await?;
            return Ok(None);
        }
        let row = sqlx::query_as::<_, OperatorApiTokenRow>(
            r"
            INSERT INTO operator_api_tokens
                (id, name, token_hash, scopes, expires_at, created_by)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, name, token_hash, scopes, expires_at, revoked_at,
                      created_by, created_at, last_used_at
            ",
        )
        .bind(new_id)
        .bind(name)
        .bind(token_hash.as_slice())
        .bind(scopes)
        .bind(expires_at)
        .bind(actor)
        .fetch_one(&mut *transaction)
        .await?;
        self.insert_operator_token_audit(
            &mut transaction,
            actor,
            "operator_token.rotate",
            old_id,
            serde_json::json!({
                "newTokenId": new_id,
                "name": name,
                "scopes": scopes,
                "expiresAt": expires_at,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok(Some(row))
    }

    /// Revokes an operator API token and records the lifecycle audit event.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the update or audit event fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn revoke_operator_api_token(
        &self,
        id: Uuid,
        actor: &str,
    ) -> Result<bool, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE operator_api_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            > 0;
        if changed {
            self.insert_operator_token_audit(
                &mut transaction,
                actor,
                "operator_token.revoke",
                id,
                serde_json::json!({}),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    async fn insert_operator_token_audit(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor: &str,
        action: &str,
        resource_id: Uuid,
        details: serde_json::Value,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            r"
            INSERT INTO audit_events
                (id, actor, action, resource_type, resource_id, correlation_id, details)
            VALUES ($1, $2, $3, 'operator_api_token', $4, $5, $6)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(actor)
        .bind(action)
        .bind(resource_id)
        .bind(Uuid::now_v7())
        .bind(details)
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }
}

/// Durable operator API token metadata and its one-way hash.
///
/// The hash is never serialized or included in the debug representation.
#[derive(Clone, FromRow)]
pub struct OperatorApiTokenRow {
    pub id: Uuid,
    pub name: String,
    pub token_hash: Vec<u8>,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for OperatorApiTokenRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorApiTokenRow")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("token_hash", &"<redacted>")
            .field("scopes", &self.scopes)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .finish()
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

/// Partial update for a source. Only `Some` fields are applied.
#[derive(Clone, Debug, Default)]
pub struct SourceUpdate {
    pub max_connections: Option<i32>,
    pub timezone: Option<String>,
    pub enabled: Option<bool>,
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
    pub refresh_interval_seconds: i32,
    pub last_refreshed_at: Option<DateTime<Utc>>,
    pub max_connections: i32,
    pub timezone: String,
    pub enabled: bool,
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
    refresh_interval_seconds: i32,
    last_refreshed_at: Option<DateTime<Utc>>,
    max_connections: i32,
    timezone: String,
    enabled: bool,
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

/// A source that is due for a scheduled refresh.
#[derive(Clone, Debug)]
pub struct DueSource {
    pub id: Uuid,
    pub kind: SourceKind,
    pub refresh_interval_seconds: i32,
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

/// One Xtream live stream selected for a bounded short EPG request.
#[derive(Clone, Debug, Eq, PartialEq, FromRow)]
pub struct XtreamShortEpgStream {
    pub stream_id: i64,
    pub channel_id: String,
}

#[derive(Debug, FromRow)]
struct DueSourceRow {
    id: Uuid,
    kind: String,
    refresh_interval_seconds: i32,
    last_refreshed_at: Option<DateTime<Utc>>,
}

impl DueSource {
    fn try_from_row(row: &DueSourceRow) -> Result<Self, PersistenceError> {
        let kind = SourceKind::from_database(&row.kind)?;
        Ok(Self {
            id: row.id,
            kind,
            refresh_interval_seconds: row.refresh_interval_seconds,
            last_refreshed_at: row.last_refreshed_at,
        })
    }
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
                       revision, updated_at, refresh_interval_seconds, last_refreshed_at,
                       max_connections, source_timezone AS timezone, enabled
                FROM provider_accounts
                WHERE enabled = true
                UNION ALL
                SELECT id, name, 'xmltv' AS kind, url_template AS endpoint,
                       revision, updated_at, refresh_interval_seconds, last_refreshed_at,
                       1 AS max_connections, timezone, enabled
                FROM epg_sources
                WHERE enabled = true
            )
            SELECT sources.id, sources.name, sources.kind, sources.endpoint,
                   sources.revision, sources.updated_at,
                   snapshot.activated_at,
                   COALESCE(snapshot.record_count, 0) AS record_count,
                   latest_job.status AS job_status,
                   sources.refresh_interval_seconds,
                   sources.last_refreshed_at,
                   sources.max_connections,
                   sources.timezone,
                   sources.enabled
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
        self.create_with_timezone(source, actor, None).await
    }

    /// Creates a source with its timezone before the initial refresh job runs.
    ///
    /// # Errors
    ///
    /// Returns validation, encryption, conflict, or database errors. The
    /// transaction is rolled back if any insert fails.
    #[allow(clippy::too_many_lines)]
    pub async fn create_with_timezone(
        &self,
        source: &NewSource,
        actor: &str,
        timezone: Option<&str>,
    ) -> Result<CreatedSource, PersistenceError> {
        validate_source(source)?;
        let timezone = validate_timezone(timezone.unwrap_or("UTC"))?;
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
                        (id, name, url_template, secret_ciphertext, timezone, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6)
                    ",
                )
                .bind(source_id)
                .bind(source.name.trim())
                .bind(&safe_endpoint)
                .bind(&ciphertext)
                .bind(&timezone)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            }
            SourceKind::M3u | SourceKind::Xtream | SourceKind::NetworkTuner => {
                sqlx::query(
                    r"
                    INSERT INTO provider_accounts
                        (id, name, source_type, base_url_template, secret_ciphertext,
                         source_timezone, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                    ",
                )
                .bind(source_id)
                .bind(source.name.trim())
                .bind(source.kind.database_name())
                .bind(&safe_endpoint)
                .bind(&ciphertext)
                .bind(&timezone)
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
                refresh_interval_seconds: 0,
                last_refreshed_at: None,
                max_connections: 1,
                timezone,
                enabled: true,
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

    /// Returns source IDs that are due for a scheduled refresh.
    ///
    /// A source is due when `refresh_interval_seconds` is greater than zero
    /// and either `last_refreshed_at` is null or the interval has elapsed
    /// since the last refresh. Sources with an active or queued refresh job
    /// are excluded.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_due_sources(&self) -> Result<Vec<DueSource>, PersistenceError> {
        let rows = sqlx::query_as::<_, DueSourceRow>(
            r"
            WITH due AS (
                SELECT id, source_type AS kind, refresh_interval_seconds, last_refreshed_at
                FROM provider_accounts
                WHERE enabled = true AND refresh_interval_seconds > 0
                  AND (last_refreshed_at IS NULL
                       OR last_refreshed_at + (refresh_interval_seconds || ' seconds')::interval <= now())
                UNION ALL
                SELECT id, 'xmltv' AS kind, refresh_interval_seconds, last_refreshed_at
                FROM epg_sources
                WHERE enabled = true AND refresh_interval_seconds > 0
                  AND (last_refreshed_at IS NULL
                       OR last_refreshed_at + (refresh_interval_seconds || ' seconds')::interval <= now())
            )
            SELECT d.id, d.kind, d.refresh_interval_seconds, d.last_refreshed_at
            FROM due d
            WHERE NOT EXISTS (
                SELECT 1 FROM jobs j
                WHERE j.kind = 'refresh-source'
                  AND j.payload->>'sourceId' = d.id::text
                  AND j.status IN ('queued', 'running')
            )
            ",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(DueSource::try_from_row).collect()
    }

    /// Updates the refresh interval for a source.
    ///
    /// Set the interval to 0 to disable automatic refresh.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::SourceNotFound`] or [`PersistenceError::Database`].
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_refresh_interval(
        &self,
        source_id: Uuid,
        interval_seconds: i32,
    ) -> Result<(), PersistenceError> {
        if interval_seconds < 0 {
            return Err(PersistenceError::SourceConflict);
        }
        let provider_result = sqlx::query(
            "UPDATE provider_accounts SET refresh_interval_seconds = $2, updated_at = now() WHERE id = $1",
        )
        .bind(source_id)
        .bind(interval_seconds)
        .execute(&self.pool)
        .await?;
        if provider_result.rows_affected() > 0 {
            return Ok(());
        }
        let epg_result = sqlx::query(
            "UPDATE epg_sources SET refresh_interval_seconds = $2, updated_at = now() WHERE id = $1",
        )
        .bind(source_id)
        .bind(interval_seconds)
        .execute(&self.pool)
        .await?;
        if epg_result.rows_affected() > 0 {
            return Ok(());
        }
        Err(PersistenceError::SourceNotFound(source_id))
    }

    /// Records the time a source refresh completed.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the update fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn mark_source_refreshed(
        &self,
        source_id: Uuid,
        refreshed_at: DateTime<Utc>,
    ) -> Result<(), PersistenceError> {
        sqlx::query("UPDATE provider_accounts SET last_refreshed_at = $2 WHERE id = $1")
            .bind(source_id)
            .bind(refreshed_at)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE epg_sources SET last_refreshed_at = $2 WHERE id = $1")
            .bind(source_id)
            .bind(refreshed_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Selects active Xtream streams for one bounded short EPG job.
    ///
    /// The effective shared-pool or account capacity limits the selection.
    /// The caller must set a small `requested_limit` for its request policy.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the selection fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_xtream_short_epg_streams(
        &self,
        source_id: Uuid,
        requested_limit: i64,
    ) -> Result<Vec<XtreamShortEpgStream>, PersistenceError> {
        let limit = requested_limit.clamp(1, 64);
        // PostgreSQL rejects bind parameters inside a LIMIT expression that
        // is wrapped in a function call. Compute the effective capacity in a
        // CTE so the LIMIT clause references a materialized scalar value.
        let rows = sqlx::query_as::<_, XtreamShortEpgStream>(
            r"
            WITH capacity AS (
                SELECT least(
                    $2::bigint,
                    coalesce(cp.max_connections, pa.max_connections)::bigint
                ) AS effective_limit
                FROM provider_accounts pa
                LEFT JOIN connection_pools cp ON cp.id = pa.connection_pool_id
                WHERE pa.id = $1
            )
            SELECT ps.provider_stream_id::bigint AS stream_id,
                   coalesce(nullif(ps.tvg_id, ''), ps.provider_stream_id) AS channel_id
            FROM provider_accounts pa
            JOIN source_snapshots ss
              ON ss.provider_account_id = pa.id
             AND ss.kind = 'xtream'
             AND ss.status = 'active'
            JOIN provider_streams ps
              ON ps.snapshot_id = ss.id
             AND ps.provider_account_id = pa.id
            CROSS JOIN capacity
            WHERE pa.id = $1
              AND pa.source_type = 'xtream'
              AND pa.enabled
              AND ps.supported
              AND ps.provider_stream_id ~ '^[0-9]+$'
            ORDER BY ps.channel_number NULLS LAST, ps.provider_stream_id
            LIMIT (SELECT effective_limit FROM capacity)
            ",
        )
        .bind(source_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Updates configurable fields on a source.
    ///
    /// Only fields that are `Some` are updated. This lets the caller
    /// patch a single field without overwriting others.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::SourceNotFound`] or [`PersistenceError::Database`].
    #[allow(clippy::missing_errors_doc)]
    pub async fn update_source(
        &self,
        source_id: Uuid,
        update: &SourceUpdate,
    ) -> Result<(), PersistenceError> {
        if let Some(timezone) = update.timezone.as_deref() {
            validate_timezone(timezone)?;
        }
        let provider_result = sqlx::query(
            "UPDATE provider_accounts
             SET max_connections = COALESCE($2, max_connections),
                 source_timezone = COALESCE($3, source_timezone),
                 enabled = COALESCE($4, enabled),
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(source_id)
        .bind(update.max_connections)
        .bind(update.timezone.as_deref())
        .bind(update.enabled)
        .execute(&self.pool)
        .await?;
        if provider_result.rows_affected() > 0 {
            return Ok(());
        }
        let epg_result = sqlx::query(
            "UPDATE epg_sources
             SET timezone = COALESCE($2, timezone),
                 enabled = COALESCE($3, enabled),
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(source_id)
        .bind(update.timezone.as_deref())
        .bind(update.enabled)
        .execute(&self.pool)
        .await?;
        if epg_result.rows_affected() > 0 {
            return Ok(());
        }
        Err(PersistenceError::SourceNotFound(source_id))
    }

    /// Deletes a source and all related data in one transaction.
    ///
    /// Removes the provider account or EPG source, cancels pending refresh
    /// jobs, and records an audit event. Child rows are deleted explicitly
    /// before the parent to avoid long cascade chains on large tables.
    ///
    /// # Errors
    ///
    /// Returns not-found or database errors.
    #[allow(clippy::missing_errors_doc)]
    pub async fn delete(&self, source_id: Uuid, actor: &str) -> Result<(), PersistenceError> {
        let mut transaction = self.pool.begin().await?;

        let existed: bool = sqlx::query_scalar(
            r"
            SELECT EXISTS (SELECT 1 FROM provider_accounts WHERE id = $1)
             OR EXISTS (SELECT 1 FROM epg_sources WHERE id = $1)
            ",
        )
        .bind(source_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !existed {
            return Err(PersistenceError::SourceNotFound(source_id));
        }

        // Cancel pending refresh jobs for this source.
        sqlx::query("UPDATE jobs SET status = 'cancelled' WHERE kind = 'refresh-source' AND payload->>'sourceId' = $1 AND status IN ('queued', 'running')")
            .bind(source_id.to_string())
            .execute(&mut *transaction)
            .await?;

        // Delete automatic channels owned by this provider. This cascades
        // to channel_streams, channel_epg_mappings, stream_profiles,
        // generated_programmes, dvr_recordings, and user_channel_access.
        sqlx::query(
            "DELETE FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic'",
        )
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;

        // Delete provider streams explicitly before the cascade from
        // source_snapshots. This avoids a deep cascade chain when the
        // provider has millions of streams. The index on
        // provider_streams(provider_account_id) makes this an index scan.
        sqlx::query("DELETE FROM provider_streams WHERE provider_account_id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;

        // Delete programmes and EPG channels through their snapshot
        // references before snapshots are removed. The indexes on
        // programmes(source_snapshot_id) and epg_channels(source_snapshot_id)
        // make these index scans instead of full table scans.
        sqlx::query(
            "DELETE FROM programmes WHERE source_snapshot_id IN (SELECT id FROM source_snapshots WHERE provider_account_id = $1)",
        )
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;

        sqlx::query(
            "DELETE FROM epg_channels WHERE source_snapshot_id IN (SELECT id FROM source_snapshots WHERE provider_account_id = $1)",
        )
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;

        // Now delete the provider account and EPG source. The remaining
        // cascades (source_snapshots, xtream_short_epg) are small.
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;

        sqlx::query("DELETE FROM epg_sources WHERE id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;

        sqlx::query(
            r"
            INSERT INTO audit_events
                (id, actor, action, resource_type, resource_id, correlation_id, details)
            VALUES ($1, $2, 'source.delete', 'source', $3, $4, $5)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(actor)
        .bind(source_id)
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({}))
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(())
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

fn validate_timezone(value: &str) -> Result<String, PersistenceError> {
    let timezone = value.trim();
    if timezone != value || timezone.is_empty() {
        return Err(PersistenceError::InvalidSource(
            "timezone must be a nonblank IANA timezone".to_owned(),
        ));
    }
    if timezone == "UTC" {
        return Ok(timezone.to_owned());
    }
    if !timezone.contains('/') {
        return Err(PersistenceError::InvalidSource(
            "timezone must be UTC or an IANA timezone".to_owned(),
        ));
    }
    Tz::from_str(timezone).map_err(|_| {
        PersistenceError::InvalidSource("timezone must be a valid IANA timezone".to_owned())
    })?;
    Ok(timezone.to_owned())
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
        refresh_interval_seconds: row.refresh_interval_seconds,
        last_refreshed_at: row.last_refreshed_at,
        max_connections: row.max_connections,
        timezone: row.timezone,
        enabled: row.enabled,
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

/// Result data from one database-elected source refresh scheduler cycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceRefreshScheduleResult {
    /// True when this worker acquired the scheduler lock for this cycle.
    pub is_leader: bool,
    /// The number of source refresh jobs that this cycle created.
    pub enqueued: u64,
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

/// A durable operator activity record for support diagnostics.
///
/// The details value can contain operational data. Callers must redact it
/// before they serialize it outside the persistence boundary.
#[derive(Clone, Debug, Deserialize, Serialize, FromRow, PartialEq)]
pub struct AuditEventRecord {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    pub correlation_id: Uuid,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

impl Database {
    /// Lists recent operator activity for redacted support diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    pub async fn list_recent_audit_events(
        &self,
        limit: u16,
    ) -> Result<Vec<AuditEventRecord>, PersistenceError> {
        let limit = i64::from(limit.clamp(1, 500));
        let records = sqlx::query_as::<_, AuditEventRecord>(
            r"
            SELECT id, actor, action, resource_type, resource_id,
                   correlation_id, details, created_at
            FROM audit_events
            ORDER BY created_at DESC, id DESC
            LIMIT $1
            ",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(records)
    }
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

    /// Enqueues one source refresh when no refresh for the source is open.
    ///
    /// The database unique index makes this operation safe for concurrent API
    /// requests and scheduler workers.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn enqueue_source_refresh_if_idle(
        &self,
        source_id: Uuid,
    ) -> Result<Option<JobRecord>, PersistenceError> {
        let source_id_text = source_id.to_string();
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, payload, available_at)
            VALUES ($1, 'refresh-source', $2, now())
            ON CONFLICT ((payload->>'sourceId'))
                WHERE kind = 'refresh-source' AND status IN ('queued', 'running')
                DO NOTHING
            RETURNING *
            ",
        )
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({ "sourceId": source_id_text }))
        .fetch_optional(&self.pool)
        .await?;
        Ok(record)
    }

    /// Elects one scheduler for this cycle and enqueues due source refreshes.
    ///
    /// The transaction advisory lock elects one worker while the query checks
    /// due sources. The partial unique index guards against an outside insert.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the transaction fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn enqueue_due_source_refreshes(
        &self,
    ) -> Result<SourceRefreshScheduleResult, PersistenceError> {
        const SOURCE_REFRESH_SCHEDULER_LOCK: i64 = 7_283_912_042;

        let mut transaction = self.pool.begin().await?;
        let is_leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(SOURCE_REFRESH_SCHEDULER_LOCK)
            .fetch_one(&mut *transaction)
            .await?;
        if !is_leader {
            transaction.rollback().await?;
            return Ok(SourceRefreshScheduleResult::default());
        }

        let due_source_ids = sqlx::query_scalar::<_, Uuid>(
            r"
            SELECT id
            FROM (
                SELECT id
                FROM provider_accounts
                WHERE enabled = true
                  AND refresh_interval_seconds > 0
                  AND (
                      last_refreshed_at IS NULL
                      OR last_refreshed_at
                         + (refresh_interval_seconds || ' seconds')::interval <= now()
                  )
                UNION ALL
                SELECT id
                FROM epg_sources
                WHERE enabled = true
                  AND refresh_interval_seconds > 0
                  AND (
                      last_refreshed_at IS NULL
                      OR last_refreshed_at
                         + (refresh_interval_seconds || ' seconds')::interval <= now()
                  )
            ) AS due_sources
            ORDER BY id
            ",
        )
        .fetch_all(&mut *transaction)
        .await?;

        let mut enqueued = 0;
        for source_id in due_source_ids {
            let source_id_text = source_id.to_string();
            let inserted = sqlx::query_scalar::<_, Uuid>(
                r"
                INSERT INTO jobs (id, kind, payload, available_at)
                VALUES ($1, 'refresh-source', $2, now())
                ON CONFLICT ((payload->>'sourceId'))
                    WHERE kind = 'refresh-source' AND status IN ('queued', 'running')
                    DO NOTHING
                RETURNING id
                ",
            )
            .bind(Uuid::now_v7())
            .bind(serde_json::json!({ "sourceId": source_id_text }))
            .fetch_optional(&mut *transaction)
            .await?;
            if inserted.is_some() {
                enqueued += 1;
            }
        }

        transaction.commit().await?;
        Ok(SourceRefreshScheduleResult {
            is_leader: true,
            enqueued,
        })
    }

    /// Enqueues one short EPG job when the source has no queued or running job.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn enqueue_xtream_short_epg(
        &self,
        source_id: Uuid,
    ) -> Result<Option<JobRecord>, PersistenceError> {
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, payload, available_at)
            SELECT $1, 'refresh-xtream-short-epg', $2, now()
            WHERE NOT EXISTS (
                SELECT 1
                FROM jobs
                WHERE kind = 'refresh-xtream-short-epg'
                  AND payload->>'sourceId' = $3
                  AND status IN ('queued', 'running')
            )
            RETURNING *
            ",
        )
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({ "sourceId": source_id.to_string() }))
        .bind(source_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(record)
    }

    /// Enqueues one provider reconciliation partition when it has no open job.
    ///
    /// The database unique index prevents duplicate worker claims after a
    /// coordinator retry. Completed jobs do not block an explicit retry.
    #[allow(clippy::missing_errors_doc)]
    pub async fn enqueue_provider_reconciliation_partition_if_idle(
        &self,
        run_id: Uuid,
        partition_number: i32,
        source_id: Uuid,
        parent_job_id: Uuid,
    ) -> Result<Option<JobRecord>, PersistenceError> {
        let run_id_text = run_id.to_string();
        let partition_text = partition_number.to_string();
        let source_id_text = source_id.to_string();
        let parent_job_id_text = parent_job_id.to_string();
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, payload, available_at)
            SELECT $1, 'reconcile-provider-partition', $2, now()
            WHERE NOT EXISTS (
                SELECT 1 FROM jobs
                WHERE id = $3 AND status = 'cancelled'
                FOR UPDATE
            )
            ON CONFLICT ((payload->>'runId'), (payload->>'partitionNumber'))
                WHERE kind = 'reconcile-provider-partition'
                  AND status IN ('queued', 'running')
                DO NOTHING
            RETURNING *
            ",
        )
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({
            "runId": run_id_text,
            "partitionNumber": partition_text,
            "sourceId": source_id_text,
            "parentJobId": parent_job_id_text,
        }))
        .bind(parent_job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(record)
    }

    /// Checks that every unfinished reconciliation partition has an open job.
    #[allow(clippy::missing_errors_doc)]
    pub async fn reconciliation_partition_jobs_are_present(
        &self,
        run_id: Uuid,
    ) -> Result<bool, PersistenceError> {
        let run_id_text = run_id.to_string();
        let complete: bool = sqlx::query_scalar(
            r"
            SELECT NOT EXISTS (
                SELECT 1
                FROM provider_reconciliation_partitions p
                WHERE p.run_id = $1
                  AND p.status IN ('queued', 'processing')
                  AND NOT EXISTS (
                      SELECT 1
                      FROM jobs j
                      WHERE j.kind = 'reconcile-provider-partition'
                        AND j.status IN ('queued', 'running')
                        AND j.payload->>'runId' = $2
                        AND j.payload->>'partitionNumber' = p.partition_number::text
                  )
            )
            ",
        )
        .bind(run_id)
        .bind(run_id_text)
        .fetch_one(&self.pool)
        .await?;
        Ok(complete)
    }

    /// Enqueues one finalizer job when no finalizer for the run is open.
    ///
    /// A finalizer can complete while partitions remain. The last completed
    /// partition then schedules a new finalizer attempt.
    #[allow(clippy::missing_errors_doc)]
    pub async fn enqueue_provider_reconciliation_finalizer_if_idle(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        parent_job_id: Uuid,
    ) -> Result<Option<JobRecord>, PersistenceError> {
        let run_id_text = run_id.to_string();
        let source_id_text = source_id.to_string();
        let parent_job_id_text = parent_job_id.to_string();
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, payload, available_at)
            SELECT $1, 'finalize-provider-reconciliation', $2, now()
            WHERE NOT EXISTS (
                SELECT 1 FROM jobs
                WHERE id = $3 AND status = 'cancelled'
                FOR UPDATE
            )
            ON CONFLICT ((payload->>'runId'))
                WHERE kind = 'finalize-provider-reconciliation'
                  AND status IN ('queued', 'running')
                DO NOTHING
            RETURNING *
            ",
        )
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({
            "runId": run_id_text,
            "sourceId": source_id_text,
            "parentJobId": parent_job_id_text,
        }))
        .bind(parent_job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(record)
    }

    /// Completes one partition and queues its finalizer in one transaction.
    ///
    /// A failed finalizer insert leaves the partition running so lease recovery retries both steps.
    #[allow(clippy::missing_errors_doc)]
    pub async fn complete_provider_reconciliation_partition(
        &self,
        job_id: Uuid,
        worker_id: &str,
        run_id: Uuid,
        source_id: Uuid,
        parent_job_id: Uuid,
    ) -> Result<(), PersistenceError> {
        let mut transaction = self.pool.begin().await?;
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
        if parent_status.is_none() {
            return Err(PersistenceError::JobNotFound(parent_job_id));
        }

        // Lock the parent before the child. Cancellation uses this same order
        // before it updates all child jobs for the source refresh.
        let updated = sqlx::query(
            r"
            UPDATE jobs
            SET status = 'succeeded', completed_at = now(), updated_at = now(),
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job_id)
        .bind(worker_id)
        .execute(&mut *transaction)
        .await?;
        ensure_owned(updated.rows_affected(), job_id, worker_id)?;

        let run_id_text = run_id.to_string();
        let source_id_text = source_id.to_string();
        let parent_job_id_text = parent_job_id.to_string();
        sqlx::query(
            r"
            INSERT INTO jobs (id, kind, payload, available_at)
            SELECT $1, 'finalize-provider-reconciliation', $2, now()
            WHERE NOT EXISTS (
                SELECT 1 FROM jobs
                WHERE id = $3 AND status = 'cancelled'
            )
            ON CONFLICT ((payload->>'runId'))
                WHERE kind = 'finalize-provider-reconciliation'
                  AND status IN ('queued', 'running')
                DO NOTHING
            ",
        )
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({
            "runId": run_id_text,
            "sourceId": source_id_text,
            "parentJobId": parent_job_id_text,
        }))
        .bind(parent_job_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Enqueues one low-priority `health-probe` job only when no queued or
    /// running `health-probe` job already targets the stream.
    ///
    /// The atomic `INSERT ... WHERE NOT EXISTS` guard prevents duplicate
    /// per-worker probes when the scheduler cycle overlaps a slow worker.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the insert fails.
    pub async fn enqueue_health_probe_if_idle(
        &self,
        provider_stream_id: Uuid,
        priority: i32,
        max_attempts: i32,
    ) -> Result<Option<JobRecord>, PersistenceError> {
        let stream_id = provider_stream_id.to_string();
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            INSERT INTO jobs (id, kind, priority, payload, max_attempts, available_at)
            SELECT $1, 'health-probe', $2, $3, $4, now()
            WHERE NOT EXISTS (
                SELECT 1
                FROM jobs
                WHERE kind = 'health-probe'
                  AND payload->>'providerStreamId' = $5
                  AND status IN ('queued', 'running')
            )
            RETURNING *
            ",
        )
        .bind(Uuid::now_v7())
        .bind(priority)
        .bind(serde_json::json!({ "providerStreamId": stream_id }))
        .bind(max_attempts)
        .bind(stream_id)
        .fetch_optional(&self.pool)
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

    /// Lists recent refresh and reconciliation jobs for one source.
    ///
    /// Child jobs carry the source identifier so control-plane views can show
    /// reconciliation progress after the refresh job stages its snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_recent_source_sync(
        &self,
        source_id: Uuid,
        limit: u16,
    ) -> Result<Vec<JobRecord>, PersistenceError> {
        let limit = i64::from(limit.clamp(1, 500));
        let source_id = source_id.to_string();
        let records = sqlx::query_as::<_, JobRecord>(
            r"
            SELECT *
            FROM jobs
            WHERE payload->>'sourceId' = $1
              AND kind IN (
                  'refresh-source',
                  'reconcile-provider-partition',
                  'finalize-provider-reconciliation'
              )
            ORDER BY created_at DESC, id DESC
            LIMIT $2
            ",
        )
        .bind(source_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(records)
    }

    /// Updates progress for a refresh that waits for child reconciliation jobs.
    ///
    /// The parent worker has returned, so the child worker owns this progress update.
    #[allow(clippy::missing_errors_doc)]
    pub async fn heartbeat_reconciliation_parent(
        &self,
        parent_job_id: Uuid,
        progress: &Value,
    ) -> Result<bool, PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET heartbeat_at = now(),
                progress = CASE
                    WHEN coalesce(progress->>'activationLocked', 'false') = 'true'
                    THEN $2 || jsonb_build_object('activationLocked', true)
                    ELSE $2
                END,
                updated_at = now()
            WHERE id = $1 AND kind = 'refresh-source' AND status = 'running'
            ",
        )
        .bind(parent_job_id)
        .bind(progress)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Completes a refresh after its reconciliation finalizer publishes the catalog.
    #[allow(clippy::missing_errors_doc)]
    pub async fn complete_reconciliation_parent(
        &self,
        parent_job_id: Uuid,
        run_id: Uuid,
        progress: &Value,
    ) -> Result<bool, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let run_id_text = run_id.to_string();
        let parent: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            r"
            SELECT status, locked_by, progress->>'runId'
            FROM jobs
            WHERE id = $1 AND kind = 'refresh-source'
            FOR UPDATE
            ",
        )
        .bind(parent_job_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((status, locked_by, parent_run_id)) = parent else {
            transaction.commit().await?;
            return Ok(false);
        };
        if !can_complete_reconciliation_parent(
            &status,
            locked_by.as_deref(),
            parent_run_id.as_deref(),
            &run_id_text,
        ) {
            transaction.commit().await?;
            return Ok(false);
        }
        sqlx::query(
            r"
            UPDATE jobs
            SET status = 'succeeded', completed_at = now(), heartbeat_at = NULL,
                locked_by = NULL, locked_at = NULL, last_error = NULL,
                progress = $2, updated_at = now()
            WHERE id = $1
            ",
        )
        .bind(parent_job_id)
        .bind(progress)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    /// Fails the source refresh when a reconciliation partition exhausts its retries.
    #[allow(clippy::missing_errors_doc)]
    pub async fn fail_reconciliation_parent_if_exhausted(
        &self,
        parent_job_id: Uuid,
        run_id: Uuid,
        progress: &Value,
    ) -> Result<bool, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
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
            transaction.commit().await?;
            return Ok(false);
        };
        if matches!(parent_status.as_str(), "cancelled" | "succeeded") {
            transaction.commit().await?;
            return Ok(false);
        }

        let run_failed = sqlx::query(
            r"
            UPDATE provider_reconciliation_runs
            SET status = 'failed', updated_at = now()
            WHERE id = $1 AND parent_job_id = $2 AND status = 'processing'
            ",
        )
        .bind(run_id)
        .bind(parent_job_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        let parent_failed = sqlx::query(
            r"
            UPDATE jobs
            SET status = 'failed', completed_at = now(), heartbeat_at = NULL,
                locked_by = NULL, locked_at = NULL, progress = $2,
                last_error = 'reconciliation partition exhausted retries', updated_at = now()
            WHERE id = $1 AND kind = 'refresh-source'
              AND status IN ('queued', 'running')
            ",
        )
        .bind(parent_job_id)
        .bind(progress)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        transaction.commit().await?;
        Ok(run_failed || parent_failed)
    }

    /// Fails the source refresh when any reconciliation child exhausts retries.
    #[allow(clippy::missing_errors_doc)]
    pub async fn fail_reconciliation_parent_for_terminal_child(
        &self,
        parent_job_id: Uuid,
        progress: &Value,
        error_summary: &str,
    ) -> Result<bool, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
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
            transaction.commit().await?;
            return Ok(false);
        };
        if matches!(parent_status.as_str(), "cancelled" | "succeeded") {
            transaction.commit().await?;
            return Ok(false);
        }

        let run_failed = sqlx::query(
            r"
            UPDATE provider_reconciliation_runs
            SET status = 'failed', updated_at = now()
            WHERE parent_job_id = $1 AND status = 'processing'
            ",
        )
        .bind(parent_job_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            > 0;
        let parent_failed = sqlx::query(
            r"
            UPDATE jobs
            SET status = 'failed', completed_at = now(), heartbeat_at = NULL,
                locked_by = NULL, locked_at = NULL, progress = $2,
                last_error = $3, updated_at = now()
            WHERE id = $1 AND kind = 'refresh-source'
              AND status IN ('queued', 'running')
            ",
        )
        .bind(parent_job_id)
        .bind(progress)
        .bind(redact_error(error_summary))
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        transaction.commit().await?;
        Ok(run_failed || parent_failed)
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
            WHERE id = $1
              AND status IN ('queued', 'running')
              AND coalesce(progress->>'activationLocked', 'false') <> 'true'
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

    /// Cancels one source refresh and its queued or running child jobs.
    ///
    /// The parent activation lock still prevents cancellation after snapshot
    /// publication starts. Child jobs never publish a partial catalog.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when cancellation cannot be persisted.
    #[allow(clippy::missing_errors_doc)]
    pub async fn cancel_source_sync(&self, parent_job_id: Uuid) -> Result<bool, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let parent_cancelled: bool = sqlx::query_scalar(
            r"
            UPDATE jobs
            SET status = 'cancelled', completed_at = now(), updated_at = now(),
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL
            WHERE id = $1
              AND status IN ('queued', 'running')
              AND coalesce(progress->>'activationLocked', 'false') <> 'true'
            RETURNING true
            ",
        )
        .bind(parent_job_id)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or(false);
        if !parent_cancelled {
            transaction.commit().await?;
            return Ok(false);
        }
        let child_count = sqlx::query(
            r"
            UPDATE jobs
            SET status = 'cancelled', completed_at = now(), updated_at = now(),
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL
            WHERE payload->>'parentJobId' = $1
              AND kind IN (
                  'reconcile-provider-partition',
                  'finalize-provider-reconciliation'
              )
              AND status IN ('queued', 'running')
              AND coalesce(progress->>'activationLocked', 'false') <> 'true'
            ",
        )
        .bind(parent_job_id.to_string())
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        sqlx::query(
            r"
            UPDATE provider_reconciliation_runs
            SET status = 'cancelled', updated_at = now()
            WHERE parent_job_id = $1 AND status IN ('queued', 'processing')
            ",
        )
        .bind(parent_job_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(parent_cancelled || child_count > 0)
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

    /// Prevents cancellation before one source snapshot transaction commits.
    ///
    /// Call this after the final cancellable staging checkpoint. The update
    /// and [`Self::cancel`] race on one row, so either cancellation wins
    /// before activation or the activation lock wins before the transaction.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobOwnership`] when cancellation or lease
    /// loss occurred before the lock.
    pub async fn begin_activation(
        &self,
        job_id: Uuid,
        worker_id: &str,
    ) -> Result<(), PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET heartbeat_at = now(),
                progress = progress || jsonb_build_object('activationLocked', true),
                updated_at = now()
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job_id)
        .bind(worker_id)
        .execute(&self.pool)
        .await?;
        ensure_owned(result.rows_affected(), job_id, worker_id)
    }

    /// Atomically claims the next available job without blocking other workers.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the claim query fails.
    pub async fn claim(&self, worker_id: &str) -> Result<Option<JobRecord>, PersistenceError> {
        let record = sqlx::query_as::<_, JobRecord>(
            r"
            UPDATE jobs
            SET status = 'running',
                locked_by = $1,
                locked_at = now(),
                heartbeat_at = now(),
                attempts = attempts + 1,
                updated_at = now()
            WHERE id = (
                SELECT id
                FROM jobs
                WHERE status = 'queued'
                  AND available_at <= now()
                  AND attempts < max_attempts
                ORDER BY priority DESC, created_at ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            RETURNING *
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
            SET heartbeat_at = now(),
                progress = CASE
                    WHEN coalesce(progress->>'activationLocked', 'false') = 'true'
                    THEN $3 || jsonb_build_object('activationLocked', true)
                    ELSE $3
                END,
                updated_at = now()
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

    /// Refreshes the heartbeat timestamp without overwriting progress.
    ///
    /// Use this during long downloads when the ingest pipeline has already
    /// reported checkpoint progress and the heartbeat only needs to keep the
    /// job lease fresh.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobOwnership`] for a stale owner and
    /// [`PersistenceError::Database`] when the update fails.
    pub async fn touch_heartbeat(
        &self,
        job_id: Uuid,
        worker_id: &str,
    ) -> Result<(), PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET heartbeat_at = now(), updated_at = now()
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job_id)
        .bind(worker_id)
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
    /// The retry delay uses exponential backoff based on the attempt count.
    /// The delay is `2^attempts` seconds, capped at 300 seconds.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::JobOwnership`] for a stale owner and
    /// [`PersistenceError::Database`] when the update fails.
    pub async fn fail(
        &self,
        job_id: Uuid,
        worker_id: &str,
        attempts: i32,
        max_attempts: i32,
        error_summary: &str,
    ) -> Result<(), PersistenceError> {
        let will_retry = attempts < max_attempts;
        let status = if will_retry { "queued" } else { "failed" };
        let delay_seconds = if will_retry {
            2_i64.pow(attempts.max(0).cast_unsigned()).min(300)
        } else {
            0
        };
        let available_at = Utc::now() + chrono::Duration::seconds(delay_seconds);
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET status = $3, last_error = $4, available_at = $5,
                completed_at = CASE WHEN $3 = 'failed' THEN now() ELSE NULL END,
                locked_by = NULL, locked_at = NULL, heartbeat_at = NULL, updated_at = now()
            WHERE id = $1 AND status = 'running' AND locked_by = $2
            ",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(status)
        .bind(redact_error(error_summary))
        .bind(available_at)
        .execute(&self.pool)
        .await?;
        ensure_owned(result.rows_affected(), job_id, worker_id)
    }

    /// Recovers stale running jobs so another worker can claim them.
    ///
    /// A job is stale when its `heartbeat_at` is older than the lease timeout.
    /// It is also stale when `heartbeat_at` is null and `locked_at` is old.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn recover_stale_jobs(
        &self,
        lease_timeout_seconds: i64,
    ) -> Result<u64, PersistenceError> {
        let result = sqlx::query(
            r"
            UPDATE jobs
            SET status = CASE
                    WHEN attempts < max_attempts THEN 'queued'
                    ELSE 'failed'
                END,
                last_error = CASE
                    WHEN attempts >= max_attempts THEN 'worker lease expired'
                    ELSE last_error
                END,
                completed_at = CASE
                    WHEN attempts >= max_attempts THEN now()
                    ELSE NULL
                END,
                locked_by = NULL,
                locked_at = NULL,
                heartbeat_at = NULL,
                updated_at = now()
            WHERE status = 'running'
              AND (
                    (
                        heartbeat_at IS NOT NULL
                        AND heartbeat_at < now() - make_interval(secs => $1)
                    )
                    OR (
                        heartbeat_at IS NULL
                        AND locked_at IS NOT NULL
                        AND locked_at < now() - make_interval(secs => $1)
                    )
              )
            ",
        )
        .bind(lease_timeout_seconds)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Lists failed jobs for operator inspection (dead-letter queue).
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_failed_jobs(&self, limit: i64) -> Result<Vec<JobRecord>, PersistenceError> {
        let rows = sqlx::query_as::<_, JobRecord>(
            r"
            SELECT * FROM jobs
            WHERE status = 'failed'
            ORDER BY updated_at DESC
            LIMIT $1
            ",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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

fn can_complete_reconciliation_parent(
    status: &str,
    locked_by: Option<&str>,
    parent_run_id: Option<&str>,
    run_id: &str,
) -> bool {
    parent_run_id == Some(run_id)
        && matches!(status, "running" | "queued" | "failed")
        && (status == "running" || locked_by.is_none())
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
    for key in [
        "username",
        "password",
        "token",
        "auth",
        "authorization",
        "secret",
        "credential",
        "cookie",
        "signature",
        "session",
        "api_key",
        "access_key",
        "key",
    ] {
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

    // Errors can include serialized request headers without query syntax.
    // Remove the complete header value because cookies can contain several
    // semicolon-separated credentials.
    for label in [
        "authorization:",
        "proxy-authorization:",
        "cookie:",
        "set-cookie:",
        "x-api-key:",
        "x-auth-token:",
        "api-key:",
    ] {
        let mut search_from = 0;
        while let Some(relative) = redacted[search_from..].to_ascii_lowercase().find(label) {
            let start = search_from + relative + label.len();
            let value_start = start
                + redacted[start..]
                    .find(|character: char| !character.is_ascii_whitespace())
                    .unwrap_or(redacted.len() - start);
            let end = redacted[value_start..]
                .find(['\r', '\n', '\'', '"'])
                .map_or(redacted.len(), |offset| value_start + offset);
            redacted.replace_range(value_start..end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        }
    }
    redacted
}

fn sensitive_diagnostic_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "username",
        "password",
        "token",
        "auth",
        "secret",
        "credential",
        "cookie",
        "header",
        "url",
        "uri",
        "endpoint",
        "processarg",
        "argv",
        "argument",
        "args",
        "command",
        "environment",
        "apikey",
        "accesskey",
        "signature",
        "sessionid",
        "key",
    ]
    .iter()
    .any(|candidate| normalized.contains(candidate))
}

/// Recursively redacts job diagnostics before they cross the control API.
pub fn redact_diagnostics(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if sensitive_diagnostic_key(key) {
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
    fn source_timezone_validation_accepts_iana_names_and_rejects_unsafe_values() {
        assert_eq!(validate_timezone("UTC").unwrap(), "UTC");
        assert_eq!(
            validate_timezone("America/Denver").unwrap(),
            "America/Denver"
        );
        for timezone in [
            "",
            " America/Denver",
            "America/Denver ",
            "MST",
            "not-a-zone",
        ] {
            assert!(validate_timezone(timezone).is_err(), "{timezone:?}");
        }
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
    fn reconciliation_parent_completion_requires_matching_run_and_safe_lease() {
        let run_id = "0192f3a8-7b6c-7d5e-8f4a-123456789abc";
        assert!(can_complete_reconciliation_parent(
            "running",
            Some("stale-worker"),
            Some(run_id),
            run_id
        ));
        assert!(can_complete_reconciliation_parent(
            "queued",
            None,
            Some(run_id),
            run_id
        ));
        assert!(can_complete_reconciliation_parent(
            "failed",
            None,
            Some(run_id),
            run_id
        ));
        assert!(!can_complete_reconciliation_parent(
            "queued",
            Some("new-worker"),
            Some(run_id),
            run_id
        ));
        assert!(!can_complete_reconciliation_parent(
            "failed",
            None,
            Some("0192f3a8-7b6c-7d5e-8f4a-123456789abd"),
            run_id
        ));
        assert!(!can_complete_reconciliation_parent(
            "cancelled",
            None,
            Some(run_id),
            run_id
        ));
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
    fn redacts_headers_cookies_and_credentials_in_plain_errors() {
        let value = redact_error(
            "request failed Authorization: Bearer provider-secret Cookie: session=output-secret",
        );
        assert!(!value.contains("provider-secret"));
        assert!(!value.contains("output-secret"));
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
    fn recursively_redacts_request_metadata_and_arbitrary_credential_fields() {
        let value = redact_diagnostics(&serde_json::json!({
            "headers": {"authorization": "Bearer provider-secret"},
            "cookies": ["session=output-secret"],
            "processArgs": ["ffmpeg", "--http-header", "Cookie: session=hidden"],
            "providerCredentials": {"username": "alice", "password": "secret"},
            "safe": {"message": "Authorization: Bearer plain-secret"}
        }));
        let rendered = value.to_string();
        for secret in [
            "provider-secret",
            "output-secret",
            "hidden",
            "alice",
            "secret",
            "plain-secret",
        ] {
            assert!(!rendered.contains(secret), "leaked {secret} in {rendered}");
        }
        assert_eq!(value["safe"]["message"], "Authorization: [REDACTED]");
    }

    #[test]
    fn ownership_guard_rejects_missing_update() {
        let job_id = Uuid::nil();
        let error = ensure_owned(0, job_id, "worker-a").unwrap_err();
        assert!(matches!(error, PersistenceError::JobOwnership { .. }));
        ensure_owned(1, job_id, "worker-a").unwrap();
    }

    #[test]
    fn source_rows_convert_states_counts_and_due_kinds() {
        let now = Utc::now();
        for (status, expected) in [
            (Some("queued"), "syncing"),
            (Some("running"), "syncing"),
            (Some("failed"), "degraded"),
            (Some("cancelled"), "offline"),
            (Some("succeeded"), "healthy"),
            (None, "healthy"),
        ] {
            let summary = source_summary_from_row(SourceSummaryRow {
                id: Uuid::nil(),
                name: "Source".to_owned(),
                kind: "m3u".to_owned(),
                endpoint: "https://provider.test/".to_owned(),
                revision: 2,
                updated_at: now,
                activated_at: None,
                record_count: -1,
                job_status: status.map(str::to_owned),
                refresh_interval_seconds: 60,
                last_refreshed_at: None,
                max_connections: 3,
                timezone: "UTC".to_owned(),
                enabled: true,
            })
            .unwrap();
            assert_eq!(summary.state, expected);
            assert_eq!(summary.channels, 0);
            assert_eq!(summary.last_sync, now);
        }

        let refreshed_at = now - chrono::Duration::minutes(5);
        let due = DueSource::try_from_row(&DueSourceRow {
            id: Uuid::nil(),
            kind: "xmltv".to_owned(),
            refresh_interval_seconds: 300,
            last_refreshed_at: Some(refreshed_at),
        })
        .unwrap();
        assert_eq!(due.kind, SourceKind::Xmltv);
        assert_eq!(due.last_refreshed_at, Some(refreshed_at));
        assert!(
            DueSource::try_from_row(&DueSourceRow {
                id: Uuid::nil(),
                kind: "unsupported".to_owned(),
                refresh_interval_seconds: 300,
                last_refreshed_at: None,
            })
            .is_err()
        );
    }

    #[test]
    fn redaction_handles_individual_fields_arrays_and_scalars() {
        let redacted = redact_error(
            "PASSWORD=first&token=second auth=third key=fourth secret=fifth username=sixth",
        );
        for secret in ["first", "second", "third", "fourth", "fifth", "sixth"] {
            assert!(!redacted.contains(secret));
        }

        let input = serde_json::json!([
            "GET HTTPS://provider.test/live?token=value failed",
            true,
            null,
            {"safe": "password=hidden"}
        ]);
        let output = redact_diagnostics(&input);
        assert_eq!(output[1], true);
        assert!(output[2].is_null());
        assert!(!output.to_string().contains("provider.test"));
        assert!(!output.to_string().contains("hidden"));
    }

    #[test]
    fn public_decryption_and_cipher_debug_keep_secrets_private() {
        let key = test_key(9);
        let plaintext = b"provider password";
        let ciphertext = key.encrypt_secret(plaintext, b"source").unwrap();
        assert_eq!(
            key.decrypt_secret(&ciphertext, b"source").unwrap(),
            plaintext
        );
        assert!(key.decrypt_secret(&ciphertext, b"other").is_err());

        let debug = format!("{:?}", SourceCipher::new(key));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("provider password"));
    }
}
