use chrono::{Duration, Utc};
use iptv_persistence::{
    CatalogRepository, Database, JobRepository, MasterKey, NewJob, NewSource, PersistenceError,
    SourceKind, SourceRepository,
};
use serde_json::json;

fn database_url() -> Option<String> {
    std::env::var("IPTV_TEST_DATABASE_URL").ok()
}

#[tokio::test]
async fn migrations_and_job_lifecycle_are_transactionally_usable() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let database = Database::connect(&database_url, 4).await.unwrap();
    database.migrate().await.unwrap();
    database.migrate().await.unwrap();
    database.health().await.unwrap();

    let repository = JobRepository::new(database.pool().clone());
    let mut first = NewJob::immediate("integration-success", json!({"fixture": true}));
    first.priority = i32::MAX;
    let enqueued = repository.enqueue(&first).await.unwrap();
    assert_eq!(enqueued.status, "queued");

    let claimed = repository
        .claim("integration-worker")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, enqueued.id);
    assert_eq!(claimed.attempts, 1);
    repository
        .heartbeat(
            claimed.id,
            "integration-worker",
            &json!({"processed": 1, "total": 1}),
        )
        .await
        .unwrap();
    assert!(matches!(
        repository
            .succeed(claimed.id, "wrong-worker")
            .await
            .unwrap_err(),
        PersistenceError::JobOwnership { .. }
    ));
    repository
        .succeed(claimed.id, "integration-worker")
        .await
        .unwrap();

    let mut failing = NewJob::immediate("integration-failure", json!({}));
    failing.priority = i32::MAX;
    failing.max_attempts = 1;
    let failing = repository.enqueue(&failing).await.unwrap();
    let claimed = repository
        .claim("integration-worker")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, failing.id);
    repository
        .fail(
            &claimed,
            "integration-worker",
            "request?username=alice&password=secret&token=value failed",
            Utc::now() + Duration::minutes(1),
        )
        .await
        .unwrap();

    let persisted: (String, Option<String>) =
        sqlx::query_as("SELECT status, last_error FROM jobs WHERE id = $1")
            .bind(failing.id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(persisted.0, "failed");
    let error = persisted.1.unwrap();
    assert!(!error.contains("alice"));
    assert!(!error.contains("secret"));
    assert!(!error.contains("value"));
}

#[tokio::test]
async fn encrypted_source_creation_enqueues_and_audits_atomically() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let database = Database::connect(&database_url, 4).await.unwrap();
    database.migrate().await.unwrap();
    let source_repository =
        SourceRepository::new(database.pool().clone(), MasterKey::from_bytes([42_u8; 32]));
    let suffix = uuid::Uuid::now_v7();
    let endpoint = "https://alice:password@provider.test/live/alice/secret/list.m3u?token=value";
    let created = source_repository
        .create(
            &NewSource {
                name: format!("Encrypted integration {suffix}"),
                kind: SourceKind::M3u,
                endpoint: endpoint.to_owned(),
            },
            "integration-test",
        )
        .await
        .unwrap();
    assert_eq!(created.source.state, "syncing");
    assert!(!created.source.endpoint.contains("alice"));
    assert_eq!(created.refresh_job.kind, "refresh-source");
    let payload = created.refresh_job.payload.to_string();
    assert!(payload.contains(&created.source.id.to_string()));
    for secret in ["alice", "password", "secret", "token", "value"] {
        assert!(!payload.contains(secret));
    }

    let stored: (String, Vec<u8>) = sqlx::query_as(
        "SELECT base_url_template, secret_ciphertext FROM provider_accounts WHERE id = $1",
    )
    .bind(created.source.id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(stored.0, created.source.endpoint);
    assert!(
        !stored
            .1
            .windows(endpoint.len())
            .any(|window| window == endpoint.as_bytes())
    );
    let decrypted = source_repository
        .load_for_job(created.source.id)
        .await
        .unwrap();
    assert_eq!(decrypted.endpoint, endpoint);
    assert_eq!(decrypted.timezone, "UTC");

    let audited: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM audit_events WHERE resource_id = $1 AND action = 'source.create')",
    )
    .bind(created.source.id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!(audited);
    let sources = source_repository.list().await.unwrap();
    assert!(sources.iter().any(|source| source.id == created.source.id));

    let jobs = JobRepository::new(database.pool().clone());
    assert!(!jobs.is_cancelled(created.refresh_job.id).await.unwrap());
    assert!(jobs.cancel(created.refresh_job.id).await.unwrap());
    assert!(jobs.is_cancelled(created.refresh_job.id).await.unwrap());
    assert!(!jobs.cancel(created.refresh_job.id).await.unwrap());
    assert!(matches!(
        jobs.cancel(uuid::Uuid::now_v7()).await,
        Err(PersistenceError::JobNotFound(_))
    ));

    let wrong_key =
        SourceRepository::new(database.pool().clone(), MasterKey::from_bytes([24_u8; 32]));
    assert!(matches!(
        wrong_key.load_for_job(created.source.id).await,
        Err(PersistenceError::Decryption)
    ));

    let mut transaction = database.pool().begin().await.unwrap();
    sqlx::query("DELETE FROM jobs WHERE payload->>'sourceId' = $1")
        .bind(created.source.id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit_events WHERE resource_id = $1")
        .bind(created.source.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
        .bind(created.source.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn migrations_preserve_conflicting_programme_metadata() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let database = Database::connect(&database_url, 4).await.unwrap();
    database.migrate().await.unwrap();
    let mut transaction = database.pool().begin().await.unwrap();
    let source_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let channel_id = uuid::Uuid::now_v7();
    let start = Utc::now();
    let stop = start + Duration::hours(1);

    sqlx::query(
        "INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, $3, false)",
    )
    .bind(source_id)
    .bind(format!("Conflict test {source_id}"))
    .bind("https://example.test/guide.xml")
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, epg_source_id, kind, status, checksum_sha256, byte_count) VALUES ($1, $2, 'xmltv', 'staging', $3, 1)",
    )
    .bind(snapshot_id)
    .bind(source_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id) VALUES ($1, $2, $3, 'channel')",
    )
    .bind(channel_id)
    .bind(snapshot_id)
    .bind(source_id)
    .execute(&mut *transaction)
    .await
    .unwrap();

    for description in ["Feed one", "Feed two"] {
        sqlx::query(
            "INSERT INTO programmes (id, source_snapshot_id, epg_channel_id, starts_at, stops_at, original_start, original_stop, title, description) VALUES ($1, $2, $3, $4, $5, $6, $7, 'Same title', $8)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(snapshot_id)
        .bind(channel_id)
        .bind(start)
        .bind(stop)
        .bind(start.to_rfc3339())
        .bind(stop.to_rfc3339())
        .bind(description)
        .execute(&mut *transaction)
        .await
        .unwrap();
    }

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM programmes WHERE source_snapshot_id = $1")
            .bind(snapshot_id)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(count, 2);
    transaction.rollback().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_merges_streams_by_tvg_id_and_maps_epg() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let database = Database::connect(&database_url, 4).await.unwrap();
    database.migrate().await.unwrap();
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let mut transaction = pool.begin().await.unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Reconcile account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 4)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();

    for (stable_key, tvg_id, name, number) in [
        ("hd", Some("news.tvg"), "News HD", Some("7.1")),
        ("sd", Some("news.tvg"), "News SD", Some("7.2")),
        ("extra", None, "Extra", Some("9.1")),
    ] {
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, $4, $5, $6, $7, 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(snapshot_id)
        .bind(account_id)
        .bind(stable_key)
        .bind(name)
        .bind(tvg_id)
        .bind(number)
        .execute(&mut *transaction)
        .await
        .unwrap();
    }
    transaction.commit().await.unwrap();

    let stats = catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();
    // Two canonical channels: one merged from the two news.tvg streams, one for Extra.
    assert_eq!(stats.channels, 2);
    assert_eq!(stats.stream_links, 3);

    let page = catalog
        .list_channels(iptv_persistence::ChannelQuery {
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(page.total >= 2);
    let news = page
        .items
        .iter()
        .find(|row| row.name == "News HD")
        .expect("merged news channel");
    assert_eq!(news.stream_count, 2);
    let news_id = news.id;

    // EPG mapping: add an active XMLTV source whose channel matches news.tvg.
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)")
        .bind(epg_source_id)
        .bind(format!("Reconcile guide {suffix}"))
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO source_snapshots (id, epg_source_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'xmltv', 'active', $3, 1, 1)")
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .bind(epg_snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id) VALUES ($1, $2, $3, 'NEWS.TVG')")
        .bind(epg_channel_id)
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO programmes (id, source_snapshot_id, epg_channel_id, starts_at, stops_at, original_start, original_stop, title) VALUES ($1, $2, $3, $4, $5, $6, $7, 'News bulletin')")
        .bind(uuid::Uuid::now_v7())
        .bind(epg_snapshot_id)
        .bind(epg_channel_id)
        .bind(Utc::now())
        .bind(Utc::now() + Duration::hours(1))
        .bind("start")
        .bind("stop")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let mapping_stats = catalog.reconcile_epg_mappings().await.unwrap();
    assert!(mapping_stats.mappings_applied >= 1);

    let mapped_page = catalog
        .list_channels(iptv_persistence::ChannelQuery {
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    let mapped_news = mapped_page
        .items
        .iter()
        .find(|row| row.id == news_id)
        .expect("news channel after mapping");
    assert!(mapped_news.epg_mapped);

    let programmes = catalog
        .list_programmes(iptv_persistence::ProgrammeQuery {
            channel_id: Some(news_id),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(programmes.total, 1);
    assert_eq!(programmes.items[0].title, "News bulletin");

    let counts = catalog.system_counts().await.unwrap();
    assert!(counts.healthy_streams >= 2);

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM channel_streams USING channels WHERE channel_streams.channel_id = channels.id AND channels.provider_account_id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM channel_epg_mappings USING channels WHERE channel_epg_mappings.channel_id = channels.id AND channels.provider_account_id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM channels WHERE provider_account_id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM epg_sources WHERE id = $1")
        .bind(epg_source_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}
