//! Integration tests for `PgSnapshotStore` against a live `PostgreSQL` instance.
//!
//! These tests require the `IPTV_TEST_DATABASE_URL` environment variable. When
//! the variable is unset, each test prints a notice and returns early so that
//! the suite remains runnable in environments without a database.

use chrono::{Duration, Utc};
use iptv_ingest::{
    ActivationProgress, IngestError, IngestFormat, PreparedEpgChannel, PreparedProgramme,
    PreparedProviderStream, PreparedSnapshot, ProtectedEndpoint, SnapshotOwner, StagedRows,
    XtreamPayloadKind,
};
use iptv_persistence::Database;
use serde_json::json;
use sqlx::PgPool;
use std::{future::Future, pin::Pin, sync::Mutex};
use uuid::Uuid;

const DATABASE_URL_ENV: &str = "IPTV_TEST_DATABASE_URL";

async fn pool() -> Option<PgPool> {
    let database_url = std::env::var(DATABASE_URL_ENV).ok()?;
    let database = Database::connect(&database_url, 4).await.expect("connect");
    database.migrate().await.expect("migrate");
    Some(database.pool().clone())
}

fn provider_stream(name: &str) -> PreparedProviderStream {
    PreparedProviderStream {
        id: Uuid::now_v7(),
        stable_key: name.to_owned(),
        provider_stream_id: None,
        name: name.to_owned(),
        group_name: Some("News".to_owned()),
        tvg_id: Some(format!("{name}.tvg")),
        tvg_name: Some(name.to_owned()),
        logo_url: None,
        channel_number: Some("7.1".to_owned()),
        endpoint: ProtectedEndpoint {
            template: "https://provider.test/{secret}".to_owned(),
            secret_ciphertext: Some(b"secret-bytes".to_vec()),
        },
        attributes: json!({}),
        directives: json!([]),
        supported: true,
    }
}

fn epg_channel(xmltv_id: &str) -> PreparedEpgChannel {
    PreparedEpgChannel {
        id: Uuid::now_v7(),
        xmltv_id: xmltv_id.to_owned(),
        display_names: json!([{"value": xmltv_id}]),
        icon_urls: json!([{"url": "https://guide.test/icon.png"}]),
        metadata: json!({}),
    }
}

fn programme(channel_id: Uuid, title: &str) -> PreparedProgramme {
    let start = Utc::now();
    PreparedProgramme {
        id: Uuid::now_v7(),
        epg_channel_id: channel_id,
        starts_at: start,
        stops_at: Some(start + Duration::hours(1)),
        original_start: start.to_rfc3339(),
        original_stop: Some((start + Duration::hours(1)).to_rfc3339()),
        title: title.to_owned(),
        subtitle: None,
        description: Some("A test programme".to_owned()),
        categories: json!(["news"]),
        metadata: json!({}),
    }
}

/// Build a snapshot owned by a provider account with the supplied streams.
fn provider_snapshot(
    account_id: Uuid,
    format: IngestFormat,
    streams: Vec<PreparedProviderStream>,
) -> PreparedSnapshot {
    let record_count = u64::try_from(streams.len()).expect("stream count fits u64");
    let mut provider_streams = StagedRows::new(1_000);
    for stream in streams {
        provider_streams.push(stream).expect("stage stream");
    }
    PreparedSnapshot {
        id: Uuid::now_v7(),
        owner: SnapshotOwner::ProviderAccount(account_id),
        format,
        checksum_sha256: format!("{:064x}", Uuid::now_v7().as_u128()),
        byte_count: 1,
        record_count,
        diagnostic_count: 0,
        diagnostics: json!([]),
        provider_streams,
        epg_channels: StagedRows::new(1_000),
        programmes: StagedRows::new(1_000),
    }
}

/// Build a snapshot owned by an EPG source with the supplied channels and programmes.
fn epg_snapshot(
    source_id: Uuid,
    channels: Vec<PreparedEpgChannel>,
    programmes: Vec<PreparedProgramme>,
) -> PreparedSnapshot {
    let record_count =
        u64::try_from(channels.len() + programmes.len()).expect("EPG record count fits u64");
    let mut epg_channels = StagedRows::new(1_000);
    for channel in channels {
        epg_channels.push(channel).expect("stage channel");
    }
    let mut staged_programmes = StagedRows::new(1_000);
    for programme in programmes {
        staged_programmes.push(programme).expect("stage programme");
    }
    PreparedSnapshot {
        id: Uuid::now_v7(),
        owner: SnapshotOwner::EpgSource(source_id),
        format: IngestFormat::Xmltv,
        checksum_sha256: format!("{:064x}", Uuid::now_v7().as_u128()),
        byte_count: 1,
        record_count,
        diagnostic_count: 0,
        diagnostics: json!([]),
        provider_streams: StagedRows::new(1_000),
        epg_channels,
        programmes: staged_programmes,
    }
}

async fn create_provider_account(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(id)
    .bind(format!("Test account {id}"))
    .execute(pool)
    .await
    .expect("insert provider account");
    id
}

async fn create_epg_source(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)",
    )
    .bind(id)
    .bind(format!("Test guide {id}"))
    .execute(pool)
    .await
    .expect("insert epg source");
    id
}

async fn snapshot_status(pool: &PgPool, snapshot_id: Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM source_snapshots WHERE id = $1")
        .bind(snapshot_id)
        .fetch_one(pool)
        .await
        .expect("fetch status");
    row.0
}

async fn active_snapshot_id(pool: &PgPool, account_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM source_snapshots WHERE provider_account_id = $1 AND kind = 'm3u' AND status = 'active'",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await
    .expect("fetch active snapshot")
}

async fn active_epg_snapshot_id(pool: &PgPool, source_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM source_snapshots WHERE epg_source_id = $1 AND kind = 'xmltv' AND status = 'active'",
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .expect("fetch active epg snapshot")
}

async fn cleanup_provider(pool: &PgPool, account_id: Uuid) {
    sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
        .bind(account_id)
        .execute(pool)
        .await
        .expect("cleanup provider");
}

async fn cleanup_epg_source(pool: &PgPool, source_id: Uuid) {
    sqlx::query("DELETE FROM epg_sources WHERE id = $1")
        .bind(source_id)
        .execute(pool)
        .await
        .expect("cleanup epg source");
}

#[tokio::test]
async fn activate_rejects_an_empty_snapshot() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    let snapshot = provider_snapshot(account_id, IngestFormat::M3u, vec![]);
    let result = store.activate(&snapshot).await;
    assert!(matches!(result, Err(IngestError::EmptySnapshot { .. })));
    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn activate_rejects_an_owner_that_does_not_match_the_format() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    let mut snapshot =
        provider_snapshot(account_id, IngestFormat::M3u, vec![provider_stream("one")]);
    snapshot.format = IngestFormat::Xmltv;
    let result = store.activate(&snapshot).await;
    assert!(matches!(result, Err(IngestError::InvalidRequest(_))));
    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn activate_persists_and_activates_a_new_m3u_snapshot() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    let snapshot = provider_snapshot(
        account_id,
        IngestFormat::M3u,
        vec![provider_stream("news"), provider_stream("sports")],
    );
    let snapshot_id = store.activate(&snapshot).await.expect("activate");
    assert_eq!(snapshot_id, snapshot.id);
    assert_eq!(snapshot_status(&pool, snapshot_id).await, "active");

    let stream_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM provider_streams WHERE snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("count streams");
    assert_eq!(stream_count, 2);

    let stored: (Option<String>, Option<Vec<u8>>) =
        sqlx::query_as("SELECT tvg_id, url_secret_ciphertext FROM provider_streams WHERE snapshot_id = $1 AND stable_key = 'news'")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("fetch stream");
    assert_eq!(stored.0.as_deref(), Some("news.tvg"));
    assert_eq!(stored.1.as_deref(), Some(b"secret-bytes".as_slice()));

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn activate_supersedes_the_previous_active_snapshot() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let first = provider_snapshot(account_id, IngestFormat::M3u, vec![provider_stream("one")]);
    let first_id = store.activate(&first).await.expect("activate first");
    assert_eq!(snapshot_status(&pool, first_id).await, "active");

    let second = provider_snapshot(account_id, IngestFormat::M3u, vec![provider_stream("two")]);
    let second_id = store.activate(&second).await.expect("activate second");
    assert_eq!(snapshot_status(&pool, second_id).await, "active");
    assert_eq!(snapshot_status(&pool, first_id).await, "superseded");
    assert_eq!(active_snapshot_id(&pool, account_id).await, Some(second_id));

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn activate_reuses_an_existing_snapshot_with_the_same_checksum() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let stream = provider_stream("one");
    let snapshot = provider_snapshot(account_id, IngestFormat::M3u, vec![stream.clone()]);
    let first_id = store.activate(&snapshot).await.expect("activate first");

    // Build a second snapshot with the same checksum but a new id.
    let mut duplicate = provider_snapshot(account_id, IngestFormat::M3u, vec![stream]);
    duplicate.checksum_sha256 = snapshot.checksum_sha256.clone();
    let reused_id = store
        .activate(&duplicate)
        .await
        .expect("activate duplicate");
    assert_eq!(reused_id, first_id);
    assert_eq!(snapshot_status(&pool, first_id).await, "active");

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn activate_persists_and_activates_a_new_xmltv_snapshot() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let source_id = create_epg_source(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let channel = epg_channel("NEWS.TVG");
    let programme = programme(channel.id, "News bulletin");
    let snapshot = epg_snapshot(source_id, vec![channel], vec![programme]);
    let snapshot_id = store.activate(&snapshot).await.expect("activate");

    assert_eq!(snapshot_status(&pool, snapshot_id).await, "active");

    let channel_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM epg_channels WHERE source_snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("count channels");
    assert_eq!(channel_count, 1);

    let programme_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM programmes WHERE source_snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("count programmes");
    assert_eq!(programme_count, 1);

    cleanup_epg_source(&pool, source_id).await;
}

#[tokio::test]
async fn activate_supersedes_the_previous_active_xmltv_snapshot() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let source_id = create_epg_source(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let first_channel = epg_channel("ONE.TVG");
    let first_programme = programme(first_channel.id, "First");
    let first = epg_snapshot(source_id, vec![first_channel], vec![first_programme]);
    let first_id = store.activate(&first).await.expect("activate first");

    let second_channel = epg_channel("TWO.TVG");
    let second_programme = programme(second_channel.id, "Second");
    let second = epg_snapshot(source_id, vec![second_channel], vec![second_programme]);
    let second_id = store.activate(&second).await.expect("activate second");

    assert_eq!(snapshot_status(&pool, second_id).await, "active");
    assert_eq!(snapshot_status(&pool, first_id).await, "superseded");
    assert_eq!(
        active_epg_snapshot_id(&pool, source_id).await,
        Some(second_id)
    );

    cleanup_epg_source(&pool, source_id).await;
}

#[tokio::test]
async fn activate_supports_xtream_provider_snapshots() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let snapshot = provider_snapshot(
        account_id,
        IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
        vec![provider_stream("live")],
    );
    let snapshot_id = store.activate(&snapshot).await.expect("activate");
    assert_eq!(snapshot_status(&pool, snapshot_id).await, "active");

    let kind: String = sqlx::query_scalar("SELECT kind FROM source_snapshots WHERE id = $1")
        .bind(snapshot_id)
        .fetch_one(&pool)
        .await
        .expect("fetch kind");
    assert_eq!(kind, "xtream");

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn pool_accessor_returns_the_inner_pool() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    sqlx::query("SELECT 1")
        .execute(store.pool())
        .await
        .expect("trivial query");
}

#[tokio::test]
async fn activate_batches_provider_streams_across_the_batch_boundary() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    // Produce more than PROVIDER_STREAM_BATCH (500) streams to exercise batching.
    let mut streams = Vec::new();
    for index in 0..750 {
        let mut stream = provider_stream(&format!("stream-{index}"));
        stream.tvg_id = Some(format!("stream-{index}.tvg"));
        streams.push(stream);
    }
    let snapshot = provider_snapshot(account_id, IngestFormat::M3u, streams);
    let snapshot_id = store.activate(&snapshot).await.expect("activate");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM provider_streams WHERE snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("count streams");
    assert_eq!(count, 750);

    cleanup_provider(&pool, account_id).await;
}

#[derive(Default)]
struct RecordingActivationProgress {
    checkpoints: Mutex<Vec<(u64, u64)>>,
}

impl ActivationProgress for RecordingActivationProgress {
    fn checkpoint(
        &self,
        records_staged: u64,
        records_total: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), IngestError>> + '_>> {
        Box::pin(async move {
            self.checkpoints
                .lock()
                .expect("lock")
                .push((records_staged, records_total));
            Ok(())
        })
    }
}

#[tokio::test]
async fn activate_reports_durable_progress_after_each_database_batch() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    let mut streams = Vec::new();
    for index in 0..750 {
        streams.push(provider_stream(&format!("progress-{index}")));
    }
    let mut snapshot = provider_snapshot(account_id, IngestFormat::M3u, streams);
    snapshot.record_count = 750;
    let progress = RecordingActivationProgress::default();

    let snapshot_id = store
        .activate_with_progress(&snapshot, &progress)
        .await
        .expect("activate");

    assert_eq!(snapshot_status(&pool, snapshot_id).await, "active");
    assert_eq!(
        progress.checkpoints.lock().expect("lock").as_slice(),
        [(500, 750), (750, 750)]
    );

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn stage_commits_immutable_rows_before_atomic_activation() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    let mut streams = StagedRows::new(1_000);
    streams
        .push(provider_stream("durable-one"))
        .expect("stage stream");
    streams
        .push(provider_stream("durable-two"))
        .expect("stage stream");
    let snapshot_id = Uuid::now_v7();
    let snapshot = PreparedSnapshot {
        id: snapshot_id,
        owner: SnapshotOwner::ProviderAccount(account_id),
        format: IngestFormat::M3u,
        checksum_sha256: format!("{:064x}", snapshot_id.as_u128()),
        byte_count: 2,
        record_count: 2,
        diagnostic_count: 0,
        diagnostics: json!([]),
        provider_streams: streams,
        epg_channels: StagedRows::new(1_000),
        programmes: StagedRows::new(1_000),
    };

    assert_eq!(store.stage(&snapshot).await.expect("stage"), snapshot_id);
    assert_eq!(snapshot_status(&pool, snapshot_id).await, "staging");
    let staged_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT staged_at FROM source_snapshots WHERE id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("staged timestamp");
    assert!(staged_at.is_some());
    let staged_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM provider_streams WHERE snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("staged rows");
    assert_eq!(staged_rows, 2);
    assert!(active_snapshot_id(&pool, account_id).await.is_none());

    assert_eq!(
        store
            .activate_staged(snapshot_id)
            .await
            .expect("activate staged"),
        snapshot_id
    );
    assert_eq!(snapshot_status(&pool, snapshot_id).await, "active");

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn staging_same_checksum_is_idempotent_and_discard_preserves_active_data() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());
    let snapshot_id = Uuid::now_v7();
    let mut streams = StagedRows::new(1_000);
    streams
        .push(provider_stream("idempotent"))
        .expect("stage stream");
    let snapshot = PreparedSnapshot {
        id: snapshot_id,
        owner: SnapshotOwner::ProviderAccount(account_id),
        format: IngestFormat::M3u,
        checksum_sha256: "idempotent-checksum".repeat(4),
        byte_count: 1,
        record_count: 1,
        diagnostic_count: 0,
        diagnostics: json!([]),
        provider_streams: streams,
        epg_channels: StagedRows::new(1_000),
        programmes: StagedRows::new(1_000),
    };
    assert_eq!(store.stage(&snapshot).await.expect("stage"), snapshot_id);
    assert_eq!(
        store.stage(&snapshot).await.expect("stage again"),
        snapshot_id
    );
    assert!(store.discard_staged(snapshot_id).await.expect("discard"));
    assert!(active_snapshot_id(&pool, account_id).await.is_none());
    assert!(
        !store
            .discard_staged(snapshot_id)
            .await
            .expect("discard again")
    );

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn failed_staged_activation_keeps_the_previous_active_snapshot() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let active = provider_snapshot(
        account_id,
        IngestFormat::M3u,
        vec![provider_stream("active")],
    );
    let active_id = active.id;
    assert_eq!(store.activate(&active).await.expect("active"), active_id);

    let staged = provider_snapshot(
        account_id,
        IngestFormat::M3u,
        vec![provider_stream("staged")],
    );
    let staged_id = staged.id;
    let mut staged = staged;
    staged.record_count = 1;
    store.stage(&staged).await.expect("stage replacement");
    sqlx::query("DELETE FROM provider_streams WHERE snapshot_id = $1")
        .bind(staged_id)
        .execute(&pool)
        .await
        .expect("remove staged row");

    assert!(store.activate_staged(staged_id).await.is_err());
    assert_eq!(active_snapshot_id(&pool, account_id).await, Some(active_id));
    assert!(store.discard_staged(staged_id).await.expect("discard"));
    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn activate_batches_programmes_across_the_batch_boundary() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let source_id = create_epg_source(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    let channel = epg_channel("BATCH.TVG");
    let channel_id = channel.id;
    let mut programmes = Vec::new();
    for index in 0..1_250 {
        programmes.push(programme(channel_id, &format!("Programme {index}")));
    }
    let snapshot = epg_snapshot(source_id, vec![channel], programmes);
    let snapshot_id = store.activate(&snapshot).await.expect("activate");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM programmes WHERE source_snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .expect("count programmes");
    assert_eq!(count, 1_250);

    cleanup_epg_source(&pool, source_id).await;
}

#[tokio::test]
async fn activate_rejects_provider_streams_without_a_provider_owner() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let source_id = create_epg_source(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    // An XMLTV snapshot that incorrectly carries provider streams.
    let mut provider_streams = StagedRows::new(1_000);
    provider_streams
        .push(provider_stream("stray"))
        .expect("stage");
    let mut snapshot = epg_snapshot(source_id, vec![], vec![]);
    snapshot.provider_streams = provider_streams;
    // Mark the snapshot as nonempty through the stray stream and keep the owner valid for XMLTV.
    let result = store.activate(&snapshot).await;
    assert!(matches!(result, Err(IngestError::InvalidRequest(_))));

    cleanup_epg_source(&pool, source_id).await;
}

#[tokio::test]
async fn activate_rejects_epg_channels_without_an_epg_source_owner() {
    let Some(pool) = pool().await else {
        eprintln!("{DATABASE_URL_ENV} is unset; skipping PostgreSQL integration test");
        return;
    };
    let account_id = create_provider_account(&pool).await;
    let store = iptv_ingest::PgSnapshotStore::new(pool.clone());

    // An M3U snapshot that incorrectly carries EPG channels.
    let mut epg_channels = StagedRows::new(1_000);
    epg_channels.push(epg_channel("STRAY.TVG")).expect("stage");
    let mut snapshot =
        provider_snapshot(account_id, IngestFormat::M3u, vec![provider_stream("one")]);
    snapshot.epg_channels = epg_channels;
    let result = store.activate(&snapshot).await;
    assert!(matches!(result, Err(IngestError::InvalidRequest(_))));

    cleanup_provider(&pool, account_id).await;
}

#[tokio::test]
async fn staged_outbox_survives_coordinator_restart_without_duplicate_jobs() {
    let Some(pool) = pool().await else {
        return;
    };
    let account = create_provider_account(&pool).await;
    let jobs = iptv_persistence::JobRepository::new(pool.clone());
    let parent = jobs
        .enqueue(&iptv_persistence::NewJob::immediate(
            "refresh-source",
            json!({"sourceId": account}),
        ))
        .await
        .expect("parent");
    let store =
        iptv_ingest::PgSnapshotStore::new(pool.clone()).with_reconciliation_parent(parent.id, 3);
    let snapshot = provider_snapshot(account, IngestFormat::M3u, vec![provider_stream("outbox")]);
    store.stage(&snapshot).await.expect("stage commits intent");
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_outbox WHERE parent_job_id = $1 AND dispatched_at IS NULL",
    )
    .bind(parent.id)
    .fetch_one(&pool)
    .await
    .expect("pending intent");
    assert_eq!(pending, 1);
    // A new repository simulates dispatch after coordinator termination.
    let recovered = iptv_persistence::JobRepository::new(pool.clone());
    recovered.dispatch_outbox().await.expect("restart dispatch");
    recovered
        .dispatch_outbox()
        .await
        .expect("idempotent dispatch");
    store.stage(&snapshot).await.expect("idempotent staging");
    recovered.dispatch_outbox().await.expect("repeat dispatch");
    let children: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE payload->>'parentJobId' = $1")
            .bind(parent.id.to_string())
            .fetch_one(&pool)
            .await
            .expect("children");
    assert_eq!(children, 3);
    sqlx::query("DELETE FROM jobs WHERE payload->>'parentJobId' = $1 OR id = $2")
        .bind(parent.id.to_string())
        .bind(parent.id)
        .execute(&pool)
        .await
        .expect("remove jobs");
    cleanup_provider(&pool, account).await;
}

#[tokio::test]
async fn rejected_staging_rolls_back_dispatch_intent() {
    let Some(pool) = pool().await else {
        return;
    };
    let account = create_provider_account(&pool).await;
    let jobs = iptv_persistence::JobRepository::new(pool.clone());
    let parent = jobs
        .enqueue(&iptv_persistence::NewJob::immediate(
            "refresh-source",
            json!({"sourceId": account}),
        ))
        .await
        .expect("parent");
    let store =
        iptv_ingest::PgSnapshotStore::new(pool.clone()).with_reconciliation_parent(parent.id, 3);
    let mut snapshot = provider_snapshot(
        account,
        IngestFormat::M3u,
        vec![provider_stream("rollback")],
    );
    snapshot.record_count = 2;
    assert!(store.stage(&snapshot).await.is_err());
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_outbox WHERE parent_job_id = $1")
            .bind(parent.id)
            .fetch_one(&pool)
            .await
            .expect("no intent");
    assert_eq!(pending, 0);
    sqlx::query("DELETE FROM jobs WHERE id = $1")
        .bind(parent.id)
        .execute(&pool)
        .await
        .expect("remove parent");
    cleanup_provider(&pool, account).await;
}
