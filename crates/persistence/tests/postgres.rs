use chrono::{Duration, TimeZone, Utc};
use iptv_persistence::{
    CatalogRepository, CreateChannelAliasInput, CreateEventTemplate, CreateRecordingInput,
    CreateRecordingRuleInput, CreateStreamProfileInput, CreateUserInput, Database,
    ENVIRONMENT_OUTPUT_PROFILE_ID, ENVIRONMENT_OUTPUT_PROFILE_NAME, EventTemplateQuery,
    EventTemplateUpdate, JobRepository, MasterKey, NewJob, NewSource, OperatorSettingOverrides,
    OutputProfileTokenHash, PersistenceError, ProgrammeQuery, SourceKind, SourceRepository,
    SourceUpdate, StreamHealthUpdate, UpdateUserInput,
};
use serde_json::{Value, json};

fn database_url() -> Option<String> {
    std::env::var("IPTV_TEST_DATABASE_URL").ok()
}

async fn isolated_database(database_url: &str) -> (Database, Database, String) {
    let admin = Database::connect(database_url, 2).await.unwrap();
    let schema = format!("iptv_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();

    let mut url = url::Url::parse(database_url).unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema},public"));
    let database = Database::connect(url.as_str(), 4).await.unwrap();
    database.migrate().await.unwrap();
    (admin, database, schema)
}

async fn drop_isolated_schema(admin: &Database, schema: &str) {
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn migrations_and_job_lifecycle_are_transactionally_usable() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
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
            claimed.id,
            "integration-worker",
            claimed.attempts,
            claimed.max_attempts,
            "request?username=alice&password=secret&token=value failed",
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

    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn bootstrap_bearer_state_is_durable_and_idempotent() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;

    assert!(database.bootstrap_bearer_enabled().await.unwrap());
    database.disable_bootstrap_bearer().await.unwrap();
    database.disable_bootstrap_bearer().await.unwrap();
    assert!(!database.bootstrap_bearer_enabled().await.unwrap());

    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn operator_api_tokens_are_hash_only_and_have_audited_lifecycle() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let first_plaintext = "first-operator-token";
    let first_hash = [7_u8; 32];
    let scopes = vec!["read".to_owned(), "control".to_owned()];
    let first_id = uuid::Uuid::now_v7();
    let created = database
        .create_operator_api_token(
            first_id,
            "integration token",
            &first_hash,
            &scopes,
            Some(Utc::now() + Duration::hours(1)),
            "integration-test",
        )
        .await
        .unwrap();
    assert_eq!(created.id, first_id);
    assert_eq!(created.token_hash, first_hash);
    let stored_hash: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM operator_api_tokens WHERE id = $1")
            .bind(first_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(stored_hash, first_hash);
    assert!(
        !stored_hash
            .windows(first_plaintext.len())
            .any(|window| window == first_plaintext.as_bytes())
    );
    let debug = format!("{created:?}");
    assert!(!debug.contains(first_plaintext));
    assert_eq!(database.list_operator_api_tokens().await.unwrap().len(), 1);

    let second_hash = [8_u8; 32];
    let second_id = uuid::Uuid::now_v7();
    let rotated = database
        .rotate_operator_api_token(
            first_id,
            second_id,
            "rotated token",
            &second_hash,
            &["admin".to_owned()],
            None,
            "integration-test",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rotated.id, second_id);
    assert_eq!(rotated.token_hash, second_hash);
    let old_revoked: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT revoked_at FROM operator_api_tokens WHERE id = $1")
            .bind(first_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert!(old_revoked.is_some());
    assert!(
        database
            .revoke_operator_api_token(second_id, "integration-test")
            .await
            .unwrap()
    );
    assert!(
        !database
            .revoke_operator_api_token(second_id, "integration-test")
            .await
            .unwrap()
    );
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE resource_type = 'operator_api_token' ORDER BY created_at, id",
    )
    .fetch_all(database.pool())
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "operator_token.create",
            "operator_token.rotate",
            "operator_token.revoke"
        ]
    );

    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn encrypted_source_creation_enqueues_and_audits_atomically() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let source_repository =
        SourceRepository::new(database.pool().clone(), MasterKey::from_bytes([42_u8; 32]));
    let suffix = uuid::Uuid::now_v7();
    let endpoint = "https://alice:password@provider.test/live/alice/secret/list.m3u?token=value";
    let created = source_repository
        .create_with_timezone(
            &NewSource {
                name: format!("Encrypted integration {suffix}"),
                kind: SourceKind::M3u,
                endpoint: endpoint.to_owned(),
            },
            "integration-test",
            Some("America/Denver"),
        )
        .await
        .unwrap();
    assert_eq!(created.source.state, "syncing");
    assert_eq!(created.source.timezone, "America/Denver");
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
    assert_eq!(decrypted.timezone, "America/Denver");

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

    drop(source_repository);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn migrations_preserve_conflicting_programme_metadata() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
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

    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_merges_streams_by_tvg_id_and_maps_epg() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
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

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_xtream_snapshots_retain_channels_and_replace_stream_links() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let account_id = uuid::Uuid::now_v7();
    let first_snapshot_id = uuid::Uuid::now_v7();
    let first_stream_id = uuid::Uuid::now_v7();

    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) \
         VALUES ($1, $2, 'xtream', 'https://provider.test/player_api.php')",
    )
    .bind(account_id)
    .bind(format!("Xtream reconciliation {}", uuid::Uuid::now_v7()))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots \
         (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
         VALUES ($1, $2, 'xtream', 'active', $3, 1, 1)",
    )
    .bind(first_snapshot_id)
    .bind(account_id)
    .bind(first_snapshot_id.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams \
         (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, url_template, \
          attributes, directives, supported) \
         VALUES ($1, $2, $3, 'xtream:101', 'Xtream News', 'news.xtream', \
                 'https://provider.test/live/101', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(first_stream_id)
    .bind(first_snapshot_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let first = catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();
    assert_eq!(first.channels, 1);
    assert_eq!(first.stream_links, 1);
    let channel_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM channels WHERE provider_account_id = $1 AND canonical_key = 'news.xtream'",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let second_snapshot_id = uuid::Uuid::now_v7();
    let second_stream_id = uuid::Uuid::now_v7();
    sqlx::query("UPDATE source_snapshots SET status = 'superseded' WHERE id = $1")
        .bind(first_snapshot_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots \
         (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
         VALUES ($1, $2, 'xtream', 'active', $3, 1, 1)",
    )
    .bind(second_snapshot_id)
    .bind(account_id)
    .bind(second_snapshot_id.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams \
         (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, url_template, \
          attributes, directives, supported) \
         VALUES ($1, $2, $3, 'xtream:102', 'Xtream News HD', 'news.xtream', \
                 'https://provider.test/live/102', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(second_stream_id)
    .bind(second_snapshot_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "CREATE FUNCTION reject_channel_stream_relink() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'relink rejection'; END; $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_channel_stream_relink_trigger \
         BEFORE INSERT ON channel_streams \
         FOR EACH ROW EXECUTE FUNCTION reject_channel_stream_relink()",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        catalog
            .reconcile_provider_account(account_id)
            .await
            .is_err()
    );
    let name_after_failure: String = sqlx::query_scalar("SELECT name FROM channels WHERE id = $1")
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name_after_failure, "Xtream News");
    let links_after_failure: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT provider_stream_id FROM channel_streams WHERE channel_id = $1")
            .bind(channel_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(links_after_failure, vec![first_stream_id]);
    sqlx::query("DROP TRIGGER reject_channel_stream_relink_trigger ON channel_streams")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION reject_channel_stream_relink()")
        .execute(&pool)
        .await
        .unwrap();

    let second = catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();
    assert_eq!(second.channels, 1);
    let retained_channel_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM channels WHERE provider_account_id = $1 AND canonical_key = 'news.xtream'",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(retained_channel_id, channel_id);
    let linked_stream_ids: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT provider_stream_id FROM channel_streams WHERE channel_id = $1")
            .bind(channel_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(linked_stream_ids, vec![second_stream_id]);

    drop(catalog);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_epg_mappings_by_channel_alias() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    // Create a provider account with a channel name that differs from the EPG name.
    // Only an explicit alias can join these names.
    let mut transaction = pool.begin().await.unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Alias-match account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, $4, $5, NULL, $6, 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(snapshot_id)
    .bind(account_id)
    .bind("arena-prime")
    .bind("Arena Prime")
    .bind("200")
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();

    // Create an EPG source with a different channel name.
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)")
        .bind(epg_source_id)
        .bind(format!("Alias-match guide {suffix}"))
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
    sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id, display_names) VALUES ($1, $2, $3, 'stadium-one.us', $4)")
        .bind(epg_channel_id)
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .bind(json!([{"value": "Stadium One"}]))
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO programmes (id, source_snapshot_id, epg_channel_id, starts_at, stops_at, original_start, original_stop, title) VALUES ($1, $2, $3, $4, $5, $6, $7, 'SportsCenter')")
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

    // First reconcile without alias — should NOT match.
    let _stats_before = catalog.reconcile_epg_mappings().await.unwrap();
    let page_before = catalog
        .list_channels(iptv_persistence::ChannelQuery {
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    let channel_before = page_before
        .items
        .iter()
        .find(|row| row.name == "Arena Prime")
        .expect("channel exists");
    assert!(
        !channel_before.epg_mapped,
        "channel should not be mapped before alias is created"
    );

    // Create a channel alias that joins both names.
    catalog
        .create_channel_alias(&iptv_persistence::CreateChannelAliasInput {
            canonical_name: "Stadium One".to_owned(),
            alias: "Arena Prime".to_owned(),
            country: None,
            category: None,
        })
        .await
        .unwrap();

    // Reconcile again — alias should now bridge the gap.
    let stats_after = catalog.reconcile_epg_mappings().await.unwrap();
    assert!(
        stats_after.mappings_applied >= 1,
        "expected at least one alias-based mapping, got {}",
        stats_after.mappings_applied
    );

    // Verify the channel is mapped and programmes are accessible.
    let page_after = catalog
        .list_channels(iptv_persistence::ChannelQuery {
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    let mapped = page_after
        .items
        .iter()
        .find(|row| row.name == "Arena Prime")
        .expect("channel exists after alias reconciliation");
    assert!(mapped.epg_mapped, "channel should be EPG-mapped by alias");

    let programmes = catalog
        .list_programmes(iptv_persistence::ProgrammeQuery {
            channel_id: Some(mapped.id),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(programmes.total, 1);
    assert_eq!(programmes.items[0].title, "SportsCenter");

    // Verify the mapping method is 'alias'.
    let mappings = catalog.list_epg_mappings(None, 100, 0).await.unwrap();
    let alias_mapping = mappings
        .items
        .iter()
        .find(|m| m.channel_name == "Arena Prime");
    assert!(
        alias_mapping.is_some(),
        "mapping should exist for Arena Prime"
    );
    assert_eq!(
        alias_mapping.unwrap().method,
        "alias",
        "mapping method should be 'alias'"
    );

    // Cleanup.
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
    sqlx::query("DELETE FROM channel_aliases WHERE canonical_name = 'Stadium One'")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_epg_mappings_by_normalized_name() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    // Create a provider account with a channel that has no tvg-id.
    // The channel name includes quality tokens that the normalizer strips.
    let mut transaction = pool.begin().await.unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Name-match account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, $4, $5, NULL, $6, 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(snapshot_id)
    .bind(account_id)
    .bind("sky-sports-action")
    .bind("UK: SKY SPORTS ACTION [H265] [720p]")
    .bind("100")
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();

    // Create an EPG source with a channel whose display name normalizes
    // to the same core name as the M3U channel.
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)")
        .bind(epg_source_id)
        .bind(format!("Name-match guide {suffix}"))
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
    sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id, display_names) VALUES ($1, $2, $3, 'SkySportsAction.uk', $4)")
        .bind(epg_channel_id)
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .bind(json!([{"value": "UK| SKY SPORTS ACTION FHD"}]))
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO programmes (id, source_snapshot_id, epg_channel_id, starts_at, stops_at, original_start, original_stop, title) VALUES ($1, $2, $3, $4, $5, $6, $7, 'Premier League')")
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
    assert!(
        mapping_stats.mappings_applied >= 1,
        "expected at least one name-based mapping, got {}",
        mapping_stats.mappings_applied
    );

    // Verify the channel is mapped and programmes are accessible.
    let page = catalog
        .list_channels(iptv_persistence::ChannelQuery {
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    let mapped = page
        .items
        .iter()
        .find(|row| row.name == "UK: SKY SPORTS ACTION [H265] [720p]")
        .expect("channel exists after reconciliation");
    assert!(mapped.epg_mapped, "channel should be EPG-mapped by name");

    let programmes = catalog
        .list_programmes(iptv_persistence::ProgrammeQuery {
            channel_id: Some(mapped.id),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(programmes.total, 1);
    assert_eq!(programmes.items[0].title, "Premier League");

    // Cleanup.
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

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stream_health_updates_quality_ranking_and_stats() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let mut transaction = pool.begin().await.unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let channel_id = uuid::Uuid::now_v7();
    let hd_stream_id = uuid::Uuid::now_v7();
    let sd_stream_id = uuid::Uuid::now_v7();

    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Health test account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 2)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO channels (id, channel_number, name, group_name, provider_account_id, canonical_key) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(channel_id)
    .bind(format!("health-{suffix}"))
    .bind("News")
    .bind("news")
    .bind(account_id)
    .bind("news")
    .execute(&mut *transaction)
    .await
    .unwrap();

    for (stream_id, name, width, height, stable_key) in [
        (hd_stream_id, "News HD", Some(1920), Some(1080), "hd"),
        (sd_stream_id, "News SD", Some(854), Some(480), "sd"),
    ] {
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, video_width, video_height, supported) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, true)",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(stable_key)
        .bind(name)
        .bind("news")
        .bind(format!("https://provider.test/{stable_key}"))
        .bind(width)
        .bind(height)
        .execute(&mut *transaction)
        .await
        .unwrap();
    }

    sqlx::query(
        "INSERT INTO channel_streams (channel_id, provider_stream_id, priority) VALUES ($1, $2, 0), ($1, $3, 1)",
    )
    .bind(channel_id)
    .bind(hd_stream_id)
    .bind(sd_stream_id)
    .execute(&mut *transaction)
    .await
    .unwrap();

    transaction.commit().await.unwrap();

    let needs_check = catalog.list_streams_needing_health_check(10).await.unwrap();
    assert_eq!(needs_check.len(), 2);

    catalog
        .update_stream_health(&StreamHealthUpdate {
            provider_stream_id: hd_stream_id,
            status: "alive".to_owned(),
            error: None,
            video_codec: Some("h264".to_owned()),
            video_resolution: Some("1920x1080".to_owned()),
            video_width: Some(1920),
            video_height: Some(1080),
            video_fps: Some(30.0),
            audio_codec: Some("aac".to_owned()),
            audio_channels: Some(2),
            audio_sample_rate: Some(48000),
            bitrate_kbps: Some(5000),
            check_duration_ms: Some(1200),
        })
        .await
        .unwrap();

    catalog
        .update_stream_health(&StreamHealthUpdate {
            provider_stream_id: sd_stream_id,
            status: "alive".to_owned(),
            error: None,
            video_codec: Some("h264".to_owned()),
            video_resolution: Some("854x480".to_owned()),
            video_width: Some(854),
            video_height: Some(480),
            video_fps: Some(30.0),
            audio_codec: Some("aac".to_owned()),
            audio_channels: Some(2),
            audio_sample_rate: Some(48000),
            bitrate_kbps: Some(1500),
            check_duration_ms: Some(900),
        })
        .await
        .unwrap();

    let page = catalog
        .list_stream_health(Some("alive"), Some("news"), 100, 0)
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert!(page.items.iter().all(|row| row.health_status == "alive"));

    let stats = catalog.stream_health_stats().await.unwrap();
    assert_eq!(stats.alive, 2);
    assert_eq!(stats.unknown, 0);

    let ranked = catalog
        .rank_channel_streams_by_quality(channel_id)
        .await
        .unwrap();
    assert_eq!(ranked, 2);

    let hd_rank: i32 = sqlx::query_scalar(
        "SELECT quality_rank FROM channel_streams WHERE channel_id = $1 AND provider_stream_id = $2",
    )
    .bind(channel_id)
    .bind(hd_stream_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let sd_rank: i32 = sqlx::query_scalar(
        "SELECT quality_rank FROM channel_streams WHERE channel_id = $1 AND provider_stream_id = $2",
    )
    .bind(channel_id)
    .bind(sd_stream_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(hd_rank < sd_rank, "HD stream should rank before SD stream");

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id IN ($1, $2)")
        .bind(hd_stream_id)
        .bind(sd_stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM channel_streams WHERE channel_id = $1")
        .bind(channel_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_streams WHERE snapshot_id = $1")
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM channels WHERE id = $1")
        .bind(channel_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn health_probe_target_loads_url_and_recovers_stranded_checking() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let mut transaction = pool.begin().await.unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let stream_id = uuid::Uuid::now_v7();
    let channel_id = uuid::Uuid::now_v7();

    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
    )
    .bind(account_id)
    .bind(format!("Probe target account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, url_secret_ciphertext, supported) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true)",
    )
    .bind(stream_id)
    .bind(snapshot_id)
    .bind(account_id)
    .bind("probe-key")
    .bind("Probe Stream")
    .bind("news")
    .bind("https://provider.test/[encrypted]")
    .bind(vec![1_u8, 2, 3, 4])
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO channels (id, channel_number, name, group_name, provider_account_id, canonical_key) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(channel_id)
    .bind(format!("probe-{suffix}"))
    .bind("Probe News")
    .bind("news")
    .bind(account_id)
    .bind("probe-news")
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO channel_streams (channel_id, provider_stream_id, priority) VALUES ($1, $2, 0)",
    )
    .bind(channel_id)
    .bind(stream_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    // The probe target must load the URL template, ciphertext, and pool data.
    let target = catalog
        .load_stream_probe_target(stream_id)
        .await
        .unwrap()
        .expect("probe target exists");
    assert_eq!(target.provider_stream_id, stream_id);
    assert_eq!(target.provider_account_id, account_id);
    assert_eq!(target.max_connections, 2);
    assert_eq!(target.input_adapter, "auto");
    assert_eq!(target.url_template, "https://provider.test/[encrypted]");
    assert_eq!(target.url_secret_ciphertext, Some(vec![1, 2, 3, 4]));
    // The Debug output must redact the URL and ciphertext.
    let debug = format!("{target:?}");
    assert!(!debug.contains("encrypted]"));
    assert!(!debug.contains("1, 2, 3"));

    // A fresh stream is `unknown` and appears in the probe list.
    let needs_probe = catalog
        .list_streams_for_health_probe(10, 300)
        .await
        .unwrap();
    assert!(
        needs_probe
            .iter()
            .any(|row| row.provider_stream_id == stream_id)
    );

    // Mark the stream as checking with an old timestamp to simulate a stranded
    // probe. The recovery method must reset it to `unknown`.
    sqlx::query(
        "UPDATE provider_streams SET health_status = 'checking', health_checked_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(stream_id)
    .execute(&pool)
    .await
    .unwrap();

    let recovered = catalog
        .recover_stranded_checking_streams(300)
        .await
        .unwrap();
    assert!(recovered >= 1);
    let status: String =
        sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "unknown");

    // update_stream_health_and_rank must persist the update and re-rank the
    // linked channel.
    catalog
        .update_stream_health_and_rank(&StreamHealthUpdate {
            provider_stream_id: stream_id,
            status: "alive".to_owned(),
            error: None,
            video_codec: Some("h264".to_owned()),
            video_resolution: None,
            video_width: Some(1920),
            video_height: Some(1080),
            video_fps: Some(30.0),
            audio_codec: Some("mp2".to_owned()),
            audio_channels: None,
            audio_sample_rate: None,
            bitrate_kbps: Some(4000),
            check_duration_ms: Some(500),
        })
        .await
        .unwrap();

    let final_status: String =
        sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(final_status, "alive");
    let rank: i32 = sqlx::query_scalar(
        "SELECT quality_rank FROM channel_streams WHERE provider_stream_id = $1",
    )
    .bind(stream_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rank, 1);

    // channels_for_stream must return the linked channel.
    let channels = catalog.channels_for_stream(stream_id).await.unwrap();
    assert_eq!(channels, vec![channel_id]);

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM channel_streams WHERE provider_stream_id = $1")
        .bind(stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
        .bind(stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_streams WHERE id = $1")
        .bind(stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM channels WHERE id = $1")
        .bind(channel_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn playback_plan_uses_ranked_candidates_and_effective_pool_caps() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let shared_pool_id = uuid::Uuid::now_v7();
    let first_account_id = uuid::Uuid::now_v7();
    let second_account_id = uuid::Uuid::now_v7();
    let third_account_id = uuid::Uuid::now_v7();
    let channel_id = uuid::Uuid::now_v7();

    sqlx::query("INSERT INTO connection_pools (id, name, max_connections) VALUES ($1, $2, 3)")
        .bind(shared_pool_id)
        .bind(format!("playback-pool-{suffix}"))
        .execute(&pool)
        .await
        .unwrap();

    for (account_id, name, adapter, connection_pool_id, max_connections) in [
        (
            first_account_id,
            "first",
            "ffmpeg",
            Some(shared_pool_id),
            1_i32,
        ),
        (
            second_account_id,
            "second",
            "vlc",
            Some(shared_pool_id),
            2_i32,
        ),
        (third_account_id, "third", "native-ts", None, 5_i32),
    ] {
        sqlx::query(
            r"
            INSERT INTO provider_accounts
                (id, name, source_type, base_url_template, connection_pool_id,
                 max_connections, input_adapter)
            VALUES ($1, $2, 'm3u', $3, $4, $5, $6)
            ",
        )
        .bind(account_id)
        .bind(format!("playback-{name}-{suffix}"))
        .bind(format!("https://provider.test/{name}"))
        .bind(connection_pool_id)
        .bind(max_connections)
        .bind(adapter)
        .execute(&pool)
        .await
        .unwrap();
    }

    sqlx::query(
        r"
        INSERT INTO channels
            (id, channel_number, name, group_name, provider_account_id,
             canonical_key, revision)
        VALUES ($1, $2, 'Playback test', 'test', $3, $4, 7)
        ",
    )
    .bind(channel_id)
    .bind(format!("playback-{suffix}"))
    .bind(first_account_id)
    .bind(format!("playback:{suffix}"))
    .execute(&pool)
    .await
    .unwrap();

    let mut stream_ids = Vec::new();
    for (index, (account_id, health, priority, quality_rank)) in [
        (first_account_id, "unknown", 0_i32, 0_i32),
        (second_account_id, "alive", 2_i32, 0_i32),
        (third_account_id, "dead", 1_i32, 0_i32),
    ]
    .into_iter()
    .enumerate()
    {
        let snapshot_id = uuid::Uuid::now_v7();
        let stream_id = uuid::Uuid::now_v7();
        stream_ids.push(stream_id);
        sqlx::query(
            r"
            INSERT INTO source_snapshots
                (id, provider_account_id, kind, status, checksum_sha256,
                 byte_count, record_count)
            VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)
            ",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("playback-{suffix}-{index}"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r"
            INSERT INTO provider_streams
                (id, snapshot_id, provider_account_id, stable_key, name,
                 url_template, supported, health_status)
            VALUES ($1, $2, $3, $4, $5, $6, true, $7)
            ",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("stream-{index}"))
        .bind(format!("Stream {index}"))
        .bind(format!("https://provider.test/stream-{index}.ts"))
        .bind(health)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r"
            INSERT INTO channel_streams
                (channel_id, provider_stream_id, priority, quality_rank)
            VALUES ($1, $2, $3, $4)
            ",
        )
        .bind(channel_id)
        .bind(stream_id)
        .bind(priority)
        .bind(quality_rank)
        .execute(&pool)
        .await
        .unwrap();
    }

    let plan = catalog
        .channel_playback_plan(channel_id)
        .await
        .unwrap()
        .expect("playback plan");
    assert_eq!(plan.channel_id, channel_id);
    assert_eq!(plan.channel_revision, 7);
    assert_eq!(plan.candidates.len(), 3);
    assert_eq!(plan.candidates[0].provider_stream_id, stream_ids[1]);
    assert_eq!(plan.candidates[0].provider_pool_id, shared_pool_id);
    assert_eq!(plan.candidates[0].max_connections, 3);
    assert_eq!(plan.candidates[0].input_adapter, "vlc");
    assert_eq!(plan.candidates[1].provider_stream_id, stream_ids[0]);
    assert_eq!(plan.candidates[1].provider_pool_id, shared_pool_id);
    assert_eq!(plan.candidates[1].max_connections, 3);
    assert_eq!(plan.candidates[2].provider_pool_id, third_account_id);
    assert_eq!(plan.candidates[2].max_connections, 5);

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn output_profiles_resolve_rotate_and_select_channels() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let channel_ids = [
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
    ];

    for (id, number) in channel_ids.into_iter().zip(["10", "20", "30"]) {
        sqlx::query("INSERT INTO channels (id, channel_number, name) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(number)
            .bind(format!("Output channel {number}"))
            .execute(&pool)
            .await
            .unwrap();
    }

    let first_hash = OutputProfileTokenHash::from_sha256([1_u8; 32]);
    let first_profile = catalog
        .ensure_environment_output_profile(first_hash.clone(), 4)
        .await
        .unwrap();
    assert_eq!(first_profile.id, ENVIRONMENT_OUTPUT_PROFILE_ID);
    assert_eq!(first_profile.name, ENVIRONMENT_OUTPUT_PROFILE_NAME);
    assert_eq!(first_profile.tuner_count, 4);
    assert!(first_profile.include_all_channels);
    assert!(first_profile.enabled);
    assert_eq!(
        catalog
            .resolve_enabled_output_profile(&first_hash)
            .await
            .unwrap()
            .map(|profile| profile.id),
        Some(ENVIRONMENT_OUTPUT_PROFILE_ID)
    );
    assert!(matches!(
        catalog
            .ensure_environment_output_profile(first_hash.clone(), 0)
            .await,
        Err(PersistenceError::InvalidOutputProfileTunerCount)
    ));

    // Re-initialize startup with the same environment hash. The tuner count
    // updates to 6 but the persisted current token hash stays as `first_hash`.
    // `ensure_environment_output_profile` must not rotate or replace the token.
    let reinitialized_profile = catalog
        .ensure_environment_output_profile(first_hash.clone(), 6)
        .await
        .unwrap();
    assert_eq!(reinitialized_profile.tuner_count, 6);
    assert_eq!(
        catalog
            .environment_output_profile_token_hash()
            .await
            .unwrap()
            .as_ref()
            .map(OutputProfileTokenHash::as_bytes),
        Some(first_hash.as_bytes())
    );
    assert_eq!(
        catalog
            .resolve_enabled_output_profile(&first_hash)
            .await
            .unwrap()
            .map(|profile| profile.id),
        Some(ENVIRONMENT_OUTPUT_PROFILE_ID)
    );

    // Rotate the token through the dedicated rotation path. The prior hash
    // stays valid through the overlap window.
    let second_hash = OutputProfileTokenHash::from_sha256([2_u8; 32]);
    let rotated_profile = catalog
        .rotate_environment_output_profile_token(second_hash.clone(), 300)
        .await
        .unwrap();
    assert_eq!(rotated_profile.id, ENVIRONMENT_OUTPUT_PROFILE_ID);
    assert_eq!(
        catalog
            .environment_output_profile_token_hash()
            .await
            .unwrap()
            .as_ref()
            .map(OutputProfileTokenHash::as_bytes),
        Some(second_hash.as_bytes())
    );
    assert!(
        catalog
            .resolve_enabled_output_profile(&first_hash)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        catalog
            .resolve_enabled_output_profile(&second_hash)
            .await
            .unwrap()
            .map(|profile| profile.id),
        Some(ENVIRONMENT_OUTPUT_PROFILE_ID)
    );

    // Simulate a restart that re-seeds the environment hash. The persisted
    // current hash must remain the rotated hash. The environment hash must not
    // become the current hash. The rotated current hash stays active.
    let startup_profile = catalog
        .ensure_environment_output_profile(first_hash.clone(), 6)
        .await
        .unwrap();
    assert_eq!(startup_profile.tuner_count, 6);
    assert_eq!(
        catalog
            .environment_output_profile_token_hash()
            .await
            .unwrap()
            .as_ref()
            .map(OutputProfileTokenHash::as_bytes),
        Some(second_hash.as_bytes())
    );
    assert_eq!(
        catalog
            .resolve_enabled_output_profile(&second_hash)
            .await
            .unwrap()
            .map(|profile| profile.id),
        Some(ENVIRONMENT_OUTPUT_PROFILE_ID)
    );
    // The environment hash still resolves only because it is the prior hash
    // within the overlap window, not because it became current.
    assert!(
        catalog
            .resolve_enabled_output_profile(&first_hash)
            .await
            .unwrap()
            .is_some()
    );

    sqlx::query(
        "UPDATE output_profiles
         SET previous_token_expires_at = now() - interval '1 second'
         WHERE id = $1",
    )
    .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        catalog
            .resolve_enabled_output_profile(&first_hash)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        catalog
            .resolve_enabled_output_profile(&second_hash)
            .await
            .unwrap()
            .map(|profile| profile.id),
        Some(ENVIRONMENT_OUTPUT_PROFILE_ID)
    );

    let all_channels = catalog
        .list_enabled_channels_for_output_profile(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .await
        .unwrap();
    assert_eq!(
        all_channels
            .iter()
            .map(|channel| channel.id)
            .collect::<Vec<_>>(),
        channel_ids
    );
    assert!(
        catalog
            .output_profile_has_channel(ENVIRONMENT_OUTPUT_PROFILE_ID, channel_ids[1])
            .await
            .unwrap()
    );
    assert!(
        !catalog
            .output_profile_has_channel(ENVIRONMENT_OUTPUT_PROFILE_ID, uuid::Uuid::now_v7())
            .await
            .unwrap()
    );

    sqlx::query("UPDATE output_profiles SET include_all_channels = false WHERE id = $1")
        .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .execute(&pool)
        .await
        .unwrap();
    for (channel_id, position) in [(channel_ids[2], 1_i32), (channel_ids[0], 0_i32)] {
        sqlx::query(
            "INSERT INTO output_profile_channels (output_profile_id, channel_id, position)
             VALUES ($1, $2, $3)",
        )
        .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .bind(channel_id)
        .bind(position)
        .execute(&pool)
        .await
        .unwrap();
    }
    let selected_channels = catalog
        .list_enabled_channels_for_output_profile(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .await
        .unwrap();
    assert_eq!(
        selected_channels
            .iter()
            .map(|channel| channel.id)
            .collect::<Vec<_>>(),
        [channel_ids[0], channel_ids[2]]
    );
    assert!(
        !catalog
            .output_profile_has_channel(ENVIRONMENT_OUTPUT_PROFILE_ID, channel_ids[1])
            .await
            .unwrap()
    );

    sqlx::query("UPDATE output_profiles SET enabled = false WHERE id = $1")
        .bind(ENVIRONMENT_OUTPUT_PROFILE_ID)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        catalog
            .resolve_enabled_output_profile(&second_hash)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        catalog
            .list_enabled_channels_for_output_profile(ENVIRONMENT_OUTPUT_PROFILE_ID)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !catalog
            .output_profile_has_channel(ENVIRONMENT_OUTPUT_PROFILE_ID, channel_ids[0])
            .await
            .unwrap()
    );

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn user_crud_and_channel_grants_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let username = format!("alice-{suffix}");
    let created = catalog
        .create_user(&CreateUserInput {
            username: username.clone(),
            display_name: "Alice".to_owned(),
            password_hash: "not-a-real-hash".to_owned(),
            role: "viewer".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(created.username, username);
    assert_eq!(created.role, "viewer");
    assert!(created.enabled);

    let by_username = catalog.get_user_by_username(&username).await.unwrap();
    assert_eq!(by_username.map(|row| row.id), Some(created.id));

    let by_id = catalog.get_user(created.id).await.unwrap();
    assert_eq!(by_id.map(|row| row.username), Some(username.clone()));

    let users = catalog.list_users().await.unwrap();
    assert!(users.iter().any(|row| row.id == created.id));

    let updated = catalog
        .update_user(
            created.id,
            &UpdateUserInput {
                display_name: Some("Alice Cooper".to_owned()),
                password_hash: None,
                role: Some("operator".to_owned()),
                enabled: None,
            },
        )
        .await
        .unwrap()
        .expect("user exists");
    assert_eq!(updated.display_name, "Alice Cooper");
    assert_eq!(updated.role, "operator");

    catalog.record_user_login(created.id).await.unwrap();
    let after_login = catalog.get_user(created.id).await.unwrap().unwrap();
    assert!(after_login.last_login_at.is_some());

    let hash = catalog
        .get_user_password_hash(&username)
        .await
        .unwrap()
        .expect("hash exists");
    assert_eq!(hash.0, created.id);
    assert_eq!(hash.1, "not-a-real-hash");

    let mut transaction = pool.begin().await.unwrap();
    let channel_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO channels (id, channel_number, name, group_name, provider_account_id, canonical_key) VALUES ($1, $2, $3, $4, NULL, $5)",
    )
    .bind(channel_id)
    .bind(format!("grant-{suffix}"))
    .bind("Grant channel")
    .bind("news")
    .bind("grant")
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    catalog
        .grant_channel_access(created.id, channel_id, None)
        .await
        .unwrap();
    let grants = catalog.list_user_channels(created.id).await.unwrap();
    assert_eq!(grants, vec![channel_id]);

    catalog
        .revoke_channel_access(created.id, channel_id)
        .await
        .unwrap();
    let empty_grants = catalog.list_user_channels(created.id).await.unwrap();
    assert!(empty_grants.is_empty());

    assert!(catalog.delete_user(created.id).await.unwrap());
    assert!(!catalog.delete_user(created.id).await.unwrap());
    assert!(catalog.get_user(created.id).await.unwrap().is_none());

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn channel_alias_crud_and_resolution_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let canonical = "Sky Sports";
    let alias = format!("Sky Sports HD {suffix}");
    let created = catalog
        .create_channel_alias(&CreateChannelAliasInput {
            canonical_name: canonical.to_owned(),
            alias: alias.clone(),
            country: Some("UK".to_owned()),
            category: Some("sports".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(created.canonical_name, canonical);
    assert_eq!(created.alias, alias);

    let page = catalog
        .list_channel_aliases(Some("UK"), 100, 0)
        .await
        .unwrap();
    assert!(page.1.iter().any(|row| row.id == created.id));

    let resolved = catalog.resolve_channel_alias(&alias).await.unwrap();
    assert_eq!(resolved, Some(canonical.to_owned()));

    let canonical_resolved = catalog.resolve_channel_alias(canonical).await.unwrap();
    assert_eq!(canonical_resolved, Some(canonical.to_owned()));

    let unknown = catalog
        .resolve_channel_alias("Not a channel")
        .await
        .unwrap();
    assert_eq!(unknown, None);

    let stats = catalog.channel_alias_stats().await.unwrap();
    assert!(stats.total >= 1);
    assert!(stats.countries >= 1);

    let duplicate = catalog
        .create_channel_alias(&CreateChannelAliasInput {
            canonical_name: canonical.to_owned(),
            alias: alias.clone(),
            country: None,
            category: None,
        })
        .await;
    assert!(
        duplicate.is_err(),
        "duplicate alias should violate unique constraint"
    );

    assert!(catalog.delete_channel_alias(created.id).await.unwrap());
    assert!(!catalog.delete_channel_alias(created.id).await.unwrap());

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn recording_crud_and_stats_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let mut transaction = pool.begin().await.unwrap();
    let channel_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO channels (id, channel_number, name, group_name, provider_account_id, canonical_key) VALUES ($1, $2, $3, $4, NULL, $5)",
    )
    .bind(channel_id)
    .bind(format!("recording-{suffix}"))
    .bind("Recording channel")
    .bind("news")
    .bind("recording")
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    let rule = catalog
        .create_recording_rule(&CreateRecordingRuleInput {
            name: format!("Evening news {suffix}"),
            channel_id,
            rule_type: "one-time".to_owned(),
            title_filter: None,
            category_filter: None,
            start_padding_minutes: 0,
            end_padding_minutes: 0,
            max_recordings: None,
            keep_until: "space-needed".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(rule.channel_id, channel_id);
    assert!(rule.enabled);

    let rules = catalog.list_recording_rules().await.unwrap();
    assert!(rules.iter().any(|row| row.id == rule.id));

    let start = Utc::now();
    let end = start + Duration::hours(1);
    let recording = catalog
        .create_recording(&CreateRecordingInput {
            rule_id: Some(rule.id),
            channel_id,
            programme_id: None,
            title: "Evening news".to_owned(),
            description: Some("Test recording".to_owned()),
            starts_at: start,
            ends_at: end,
        })
        .await
        .unwrap();
    assert_eq!(recording.status, "scheduled");

    let page = catalog.list_recordings(None, 100, 0).await.unwrap();
    assert!(page.1.iter().any(|row| row.id == recording.id));

    let completed = catalog
        .update_recording_status(
            recording.id,
            "completed",
            Some("/recordings/test.ts"),
            Some(1_000_000),
            Some(3600),
            None,
        )
        .await
        .unwrap()
        .expect("recording exists");
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.file_size_bytes, Some(1_000_000));

    let stats = catalog.recording_stats().await.unwrap();
    assert_eq!(stats.completed, 1);
    assert_eq!(stats.total_bytes, 1_000_000);

    assert!(catalog.delete_recording(recording.id).await.unwrap());
    assert!(!catalog.delete_recording(recording.id).await.unwrap());

    let stats_after_delete = catalog.recording_stats().await.unwrap();
    assert_eq!(stats_after_delete.completed, 0);

    assert!(catalog.delete_recording_rule(rule.id).await.unwrap());

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM channels WHERE id = $1")
        .bind(channel_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stream_profile_crud_and_assignment_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let mut transaction = pool.begin().await.unwrap();
    let channel_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO channels (id, channel_number, name, group_name, provider_account_id, canonical_key) VALUES ($1, $2, $3, $4, NULL, $5)",
    )
    .bind(channel_id)
    .bind(format!("profile-{suffix}"))
    .bind("Profile channel")
    .bind("news")
    .bind("profile")
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    let profile = catalog
        .create_stream_profile(&CreateStreamProfileInput {
            name: format!("FFmpeg proxy {suffix}"),
            profile_type: "ffmpeg".to_owned(),
            command: Some("ffmpeg".to_owned()),
            arguments: json!(["-i", "{url}"]),
            buffer_seconds: 3.0,
            user_agent: None,
            referer: None,
        })
        .await
        .unwrap();
    assert_eq!(profile.profile_type, "ffmpeg");
    assert!(profile.enabled);

    let profiles = catalog.list_stream_profiles().await.unwrap();
    assert!(profiles.iter().any(|row| row.id == profile.id));

    catalog
        .assign_stream_profile(channel_id, profile.id)
        .await
        .unwrap();

    let assigned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_stream_profiles WHERE channel_id = $1 AND stream_profile_id = $2",
    )
    .bind(channel_id)
    .bind(profile.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(assigned, 1);

    catalog
        .remove_stream_profile(channel_id, profile.id)
        .await
        .unwrap();
    let removed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channel_stream_profiles WHERE channel_id = $1")
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(removed, 0);

    catalog
        .assign_stream_profile(channel_id, profile.id)
        .await
        .unwrap();
    catalog
        .remove_stream_profile(channel_id, uuid::Uuid::nil())
        .await
        .unwrap();
    let all_removed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channel_stream_profiles WHERE channel_id = $1")
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(all_removed, 0);

    assert!(catalog.delete_stream_profile(profile.id).await.unwrap());
    assert!(!catalog.delete_stream_profile(profile.id).await.unwrap());

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn catalog_browse_and_manual_mapping_queries_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let channel_id = uuid::Uuid::now_v7();
    let epg_source_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let programme_id = uuid::Uuid::now_v7();
    let channel_number = format!("browse-{suffix}");
    let channel_name = format!("Browse channel {suffix}");
    let group_name = format!("Browse group {suffix}");
    let epg_name = format!("Browse guide {suffix}");
    let reference_now = Utc.with_ymd_and_hms(2026, 7, 1, 18, 0, 0).single().unwrap();
    let starts_at = reference_now + Duration::minutes(5);
    let stops_at = starts_at + Duration::hours(1);

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO channels
            (id, channel_number, name, group_name, managed_by, canonical_key)
         VALUES ($1, $2, $3, $4, 'manual', $5)",
    )
    .bind(channel_id)
    .bind(&channel_number)
    .bind(&channel_name)
    .bind(&group_name)
    .bind(format!("browse-{suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_sources (id, name, url_template, enabled)
         VALUES ($1, $2, 'https://example.test/guide.xml', true)",
    )
    .bind(epg_source_id)
    .bind(&epg_name)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots
            (id, epg_source_id, kind, status, checksum_sha256, byte_count,
             record_count, activated_at)
         VALUES ($1, $2, 'xmltv', 'active', $3, 1, 1, now())",
    )
    .bind(snapshot_id)
    .bind(epg_source_id)
    .bind(format!("browse-{suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_channels
            (id, source_snapshot_id, epg_source_id, xmltv_id, display_names)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(epg_channel_id)
    .bind(snapshot_id)
    .bind(epg_source_id)
    .bind(format!("xmltv-{suffix}"))
    .bind(json!([{"value": channel_name}]))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO programmes
            (id, source_snapshot_id, epg_channel_id, starts_at, stops_at,
             original_start, original_stop, title, categories)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(programme_id)
    .bind(snapshot_id)
    .bind(epg_channel_id)
    .bind(starts_at)
    .bind(stops_at)
    .bind(starts_at.to_rfc3339())
    .bind(stops_at.to_rfc3339())
    .bind(format!("Browse programme {suffix}"))
    .bind(json!(["News", "Local"]))
    .execute(&mut *transaction)
    .await
    .unwrap();
    for (id, start, stop, title) in [
        (
            uuid::Uuid::now_v7(),
            reference_now - Duration::minutes(5),
            reference_now + Duration::minutes(5),
            format!("Current programme {suffix}"),
        ),
        (
            uuid::Uuid::now_v7(),
            reference_now - Duration::hours(2),
            reference_now - Duration::hours(1),
            format!("Past programme {suffix}"),
        ),
    ] {
        sqlx::query(
            "INSERT INTO programmes
                (id, source_snapshot_id, epg_channel_id, starts_at, stops_at,
                 original_start, original_stop, title, categories)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(snapshot_id)
        .bind(epg_channel_id)
        .bind(start)
        .bind(stop)
        .bind(start.to_rfc3339())
        .bind(stop.to_rfc3339())
        .bind(title)
        .bind(json!(["News"]))
        .execute(&mut *transaction)
        .await
        .unwrap();
    }
    transaction.commit().await.unwrap();

    let page = catalog
        .list_channels(iptv_persistence::ChannelQuery {
            search: Some(suffix.to_string()),
            group: Some(group_name.clone()),
            enabled: Some(true),
            limit: Some(5),
            offset: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, channel_id);
    assert!(!page.items[0].epg_mapped);

    let groups = catalog.list_groups().await.unwrap();
    let group = groups.iter().find(|row| row.name == group_name).unwrap();
    assert_eq!(group.channel_count, 1);
    assert_eq!(group.enabled_count, 1);

    let matches = catalog
        .search_epg_channels(&suffix.to_string(), 0)
        .await
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, epg_channel_id);

    assert!(matches!(
        catalog
            .resolve_review(channel_id, true, None, "integration-test")
            .await,
        Err(PersistenceError::Database(_))
    ));
    catalog
        .set_manual_epg_mapping(channel_id, epg_channel_id, "integration-test")
        .await
        .unwrap();

    let programmes = catalog
        .list_programmes(ProgrammeQuery {
            channel_id: Some(channel_id),
            from: None,
            search: Some(suffix.to_string()),
            now: Some(reference_now),
            limit: Some(5),
            offset: Some(0),
        })
        .await
        .unwrap();
    assert_eq!(programmes.total, 3);
    assert!(programmes.items[0].title.starts_with("Current programme"));
    assert!(programmes.items[1].title.starts_with("Browse programme"));
    assert!(programmes.items[2].title.starts_with("Past programme"));
    assert_eq!(programmes.items[0].category_list(), ["News"]);

    let output_programmes = catalog
        .list_programmes_for_channels(&[channel_id])
        .await
        .unwrap();
    assert_eq!(output_programmes.len(), 3);
    assert!(
        catalog
            .list_programmes_for_channels(&[])
            .await
            .unwrap()
            .is_empty()
    );

    let counts = catalog.system_counts().await.unwrap();
    assert!(counts.channels >= 1);
    assert!(counts.guide_coverage > 0.0);

    assert_eq!(
        catalog
            .set_channel_enabled(channel_id, false)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        catalog.set_group_enabled(&group_name, true).await.unwrap(),
        1
    );
    catalog
        .resolve_review(channel_id, true, Some(epg_channel_id), "integration-test")
        .await
        .unwrap();
    catalog
        .resolve_review(channel_id, false, None, "integration-test")
        .await
        .unwrap();
    catalog.remove_epg_mapping(channel_id).await.unwrap();

    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn event_template_and_channel_lifecycle_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let catalog = CatalogRepository::new(database.pool().clone());
    let suffix = uuid::Uuid::now_v7();
    let original_name = format!("event-{suffix}");
    let updated_name = format!("updated-event-{suffix}");
    let template = catalog
        .create_event_template(CreateEventTemplate {
            name: original_name.clone(),
            display_name: "Test league".to_owned(),
            match_regex: format!("No stream can match {suffix}"),
            channel_name_format: "{home} vs {away}".to_owned(),
            group_name: format!("Event group {suffix}"),
            event_duration_hours: 3,
            past_date_grace_hours: 4,
            future_date_days: 2,
            timezone: "America/Denver".to_owned(),
            filler_title: "No coverage".to_owned(),
        })
        .await
        .unwrap();

    let enabled = catalog
        .list_event_templates(EventTemplateQuery { enabled_only: true })
        .await
        .unwrap();
    assert!(enabled.iter().any(|row| row.id == template.id));

    let updated = catalog
        .update_event_template(
            template.id,
            CreateEventTemplate {
                name: updated_name.clone(),
                display_name: "Updated league".to_owned(),
                match_regex: format!("Still no stream can match {suffix}"),
                channel_name_format: "{away} at {home}".to_owned(),
                group_name: format!("Updated event group {suffix}"),
                event_duration_hours: 4,
                past_date_grace_hours: 5,
                future_date_days: 3,
                timezone: "America/New_York".to_owned(),
                filler_title: "Off air".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, updated_name);
    assert_eq!(updated.timezone, "America/New_York");
    assert_eq!(updated.filler_title, "Off air");
    assert!(matches!(
        catalog
            .update_event_template(uuid::Uuid::now_v7(), CreateEventTemplate::default())
            .await,
        Err(PersistenceError::SourceNotFound(_))
    ));

    let start = Utc::now() - Duration::hours(3);
    let end = start + Duration::hours(1);
    let event = catalog
        .upsert_event_channel(
            template.id,
            7,
            Some("Home vs Away"),
            Some(start),
            Some(end),
            "Provider event 7",
            "scheduled",
        )
        .await
        .unwrap();
    let changed = catalog
        .upsert_event_channel(
            template.id,
            7,
            Some("Away at Home"),
            Some(start),
            Some(end),
            "Updated provider event 7",
            "active",
        )
        .await
        .unwrap();
    assert_eq!(changed.id, event.id);
    assert_eq!(changed.state, "active");

    let channel_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO channels
            (id, channel_number, name, group_name, managed_by, canonical_key)
         VALUES ($1, $2, $3, $4, 'event', $5)",
    )
    .bind(channel_id)
    .bind(format!("event-{suffix}"))
    .bind("Dynamic test channel")
    .bind(&updated.group_name)
    .bind(format!("event-channel-{suffix}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE event_channels SET channel_id = $2, state = 'scheduled' WHERE id = $1")
        .bind(event.id)
        .bind(channel_id)
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO generated_programmes
            (id, event_channel_id, channel_id, template_id, rule_id, rule_name,
             stable_event_key, source_title, kind, starts_at, stops_at, title, categories)
         VALUES ($1, $2, $3, $4, $4, 'Updated league', 'updated-event',
                 'Updated provider event 7', 'event', $5, $6, 'Away at Home', $7)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(event.id)
    .bind(channel_id)
    .bind(template.id)
    .bind(start)
    .bind(end)
    .bind(json!(["Dynamic event", "event"]))
    .execute(database.pool())
    .await
    .unwrap();
    let output_programmes = catalog
        .list_programmes_for_channels(&[channel_id])
        .await
        .unwrap();
    assert_eq!(output_programmes.len(), 1);
    assert_eq!(output_programmes[0].title, "Away at Home");
    assert_eq!(
        output_programmes[0].source_name.as_deref(),
        Some("Updated league")
    );
    assert_eq!(
        output_programmes[0].category_list(),
        ["Dynamic event", "event"]
    );

    let events = catalog
        .list_event_channels(Some(template.id))
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(catalog.scan_event_channels(template.id).await.unwrap(), 0);
    assert!(matches!(
        catalog.scan_event_channels(uuid::Uuid::now_v7()).await,
        Err(PersistenceError::SourceNotFound(_))
    ));
    assert_eq!(catalog.prune_past_event_channels(0).await.unwrap(), 1);
    let events = catalog.list_event_channels(None).await.unwrap();
    assert!(
        events
            .iter()
            .any(|row| row.id == event.id && row.state == "hidden")
    );

    assert_eq!(catalog.delete_event_template(template.id).await.unwrap(), 1);
    assert_eq!(catalog.delete_event_template(template.id).await.unwrap(), 0);
    drop(catalog);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn event_template_partial_update_persists_enabled_and_optional_fields() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let catalog = CatalogRepository::new(database.pool().clone());
    let suffix = uuid::Uuid::now_v7();
    let template = catalog
        .create_event_template(CreateEventTemplate {
            name: format!("partial-{suffix}"),
            display_name: "Partial league".to_owned(),
            match_regex: format!("Partial {suffix}"),
            channel_name_format: "{home} vs {away}".to_owned(),
            group_name: format!("Partial group {suffix}"),
            event_duration_hours: 3,
            past_date_grace_hours: 4,
            future_date_days: 2,
            timezone: "UTC".to_owned(),
            filler_title: "No programs available".to_owned(),
        })
        .await
        .unwrap();
    assert!(template.enabled);

    // Disable the template and change only the duration.
    let disabled = catalog
        .update_event_template_partial(
            template.id,
            &EventTemplateUpdate {
                enabled: Some(false),
                event_duration_hours: Some(6),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.event_duration_hours, 6);
    assert_eq!(disabled.timezone, "UTC");
    assert_eq!(disabled.filler_title, "No programs available");
    // Omitted fields keep their stored values.
    assert_eq!(disabled.display_name, "Partial league");
    assert_eq!(disabled.past_date_grace_hours, 4);
    assert_eq!(disabled.future_date_days, 2);

    // The disabled template no longer appears in the enabled-only listing.
    let enabled_list = catalog
        .list_event_templates(EventTemplateQuery { enabled_only: true })
        .await
        .unwrap();
    assert!(enabled_list.iter().all(|row| row.id != template.id));

    // Empty update leaves all fields unchanged.
    let unchanged = catalog
        .update_event_template_partial(template.id, &EventTemplateUpdate::default())
        .await
        .unwrap();
    assert!(!unchanged.enabled);
    assert_eq!(unchanged.event_duration_hours, 6);

    // Re-enable and rename in one partial update.
    let reenabled = catalog
        .update_event_template_partial(
            template.id,
            &EventTemplateUpdate {
                enabled: Some(true),
                display_name: Some("Renamed league".to_owned()),
                future_date_days: Some(9),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(reenabled.enabled);
    assert_eq!(reenabled.display_name, "Renamed league");
    assert_eq!(reenabled.future_date_days, 9);

    // Unknown ID returns SourceNotFound.
    assert!(matches!(
        catalog
            .update_event_template_partial(
                uuid::Uuid::now_v7(),
                &EventTemplateUpdate {
                    enabled: Some(false),
                    ..Default::default()
                }
            )
            .await,
        Err(PersistenceError::SourceNotFound(_))
    ));

    assert_eq!(catalog.delete_event_template(template.id).await.unwrap(), 1);
    drop(catalog);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn event_scan_persists_idempotent_non_overlapping_generated_guides() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let catalog = CatalogRepository::new(database.pool().clone());
    let suffix = uuid::Uuid::now_v7();
    let group = format!("Dynamic events {suffix}");
    let channel_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO channels
            (id, channel_number, name, group_name, managed_by, canonical_key)
         VALUES ($1, '700', 'Dynamic event channel', $2, 'event', $3)",
    )
    .bind(channel_id)
    .bind(&group)
    .bind(format!("dynamic-{suffix}"))
    .execute(database.pool())
    .await
    .unwrap();
    let template = catalog
        .create_event_template(CreateEventTemplate {
            name: format!("generated-{suffix}"),
            display_name: "Generated sports".to_owned(),
            match_regex: "Broncos|Rams".to_owned(),
            channel_name_format: "{home} vs {away}".to_owned(),
            group_name: group.clone(),
            event_duration_hours: 3,
            past_date_grace_hours: 1,
            future_date_days: 1,
            timezone: "America/Denver".to_owned(),
            filler_title: "Off air".to_owned(),
        })
        .await
        .unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template)
         VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("generated-{suffix}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots
            (id, provider_account_id, kind, status, checksum_sha256, byte_count, activated_at)
         VALUES ($1, $2, 'm3u', 'active', $3, 1, now())",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(format!("generated-{suffix}"))
    .execute(database.pool())
    .await
    .unwrap();
    for (stable_key, name) in [
        ("first", "Event 7: Broncos vs Chiefs 2026-09-14 00:20 HD"),
        ("second", "Event 7: Rams vs Seahawks 2026-09-14 05:20 HD"),
    ] {
        sqlx::query(
            "INSERT INTO provider_streams
                (id, snapshot_id, provider_account_id, stable_key, name, group_name,
                 url_template, attributes, directives, supported)
             VALUES ($1, $2, $3, $4, $5, 'Sports', 'https://provider.test/stream',
                     '{}'::jsonb, '[]'::jsonb, true)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(snapshot_id)
        .bind(account_id)
        .bind(stable_key)
        .bind(name)
        .execute(database.pool())
        .await
        .unwrap();
    }

    assert_eq!(catalog.scan_event_channels(template.id).await.unwrap(), 1);
    let first = catalog
        .list_programmes_for_channels(&[channel_id])
        .await
        .unwrap();
    let api_programmes = catalog
        .list_programmes(ProgrammeQuery {
            channel_id: Some(channel_id),
            limit: Some(100),
            now: Some(
                chrono::DateTime::parse_from_rfc3339("2026-09-13T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            ..ProgrammeQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(
        api_programmes.total,
        i64::try_from(first.len()).expect("programme count fits in i64")
    );
    assert!(
        api_programmes
            .items
            .iter()
            .any(|programme| programme.title == "Broncos vs Chiefs")
    );
    assert_eq!(first.len(), 5);
    assert_eq!(
        first
            .iter()
            .filter(|programme| programme.title == "Off air")
            .count(),
        3
    );
    assert!(
        first
            .windows(2)
            .all(|pair| pair[0].stops_at == pair[1].starts_at)
    );
    assert!(
        first
            .windows(2)
            .all(|pair| pair[0].stops_at <= pair[1].starts_at)
    );
    let event_titles: Vec<_> = first
        .iter()
        .filter(|programme| programme.category_list().contains(&"event".to_owned()))
        .map(|programme| programme.title.as_str())
        .collect();
    assert_eq!(event_titles, ["Broncos vs Chiefs", "Rams vs Seahawks"]);
    let first_event = first
        .iter()
        .find(|programme| programme.title == "Broncos vs Chiefs")
        .expect("first event programme");
    assert_eq!(
        first_event.starts_at,
        chrono::DateTime::parse_from_rfc3339("2026-09-14T06:20:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    assert!(first.iter().all(|programme| {
        programme.description.is_some()
            && programme
                .source_name
                .as_deref()
                .is_some_and(|name| name.contains("Generated"))
    }));

    let conflicting_template = catalog
        .create_event_template(CreateEventTemplate {
            name: format!("conflicting-{suffix}"),
            display_name: "Conflicting sports".to_owned(),
            match_regex: "Broncos|Rams".to_owned(),
            channel_name_format: "{home} vs {away}".to_owned(),
            group_name: group,
            event_duration_hours: 3,
            past_date_grace_hours: 1,
            future_date_days: 1,
            timezone: "UTC".to_owned(),
            filler_title: "No programs available".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(
        catalog
            .scan_event_channels(conflicting_template.id)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        catalog
            .list_programmes_for_channels(&[channel_id])
            .await
            .unwrap()
            .iter()
            .map(|programme| (&programme.title, programme.starts_at, programme.stops_at))
            .collect::<Vec<_>>(),
        first
            .iter()
            .map(|programme| (&programme.title, programme.starts_at, programme.stops_at))
            .collect::<Vec<_>>()
    );

    assert_eq!(catalog.scan_all_event_channels().await.unwrap(), 1);
    let second = catalog
        .list_programmes_for_channels(&[channel_id])
        .await
        .unwrap();
    assert_eq!(
        first
            .iter()
            .map(|programme| (&programme.title, programme.starts_at, programme.stops_at))
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|programme| (&programme.title, programme.starts_at, programme.stops_at))
            .collect::<Vec<_>>()
    );

    sqlx::query("DELETE FROM provider_streams WHERE snapshot_id = $1")
        .bind(snapshot_id)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(catalog.scan_event_channels(template.id).await.unwrap(), 0);
    let preserved = catalog
        .list_programmes_for_channels(&[channel_id])
        .await
        .unwrap();
    assert_eq!(preserved.len(), second.len());
    drop(catalog);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn source_configuration_and_job_management_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([31_u8; 32]));
    let jobs = JobRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let provider = sources
        .create(
            &NewSource {
                name: format!("Provider {suffix}"),
                kind: SourceKind::M3u,
                endpoint: "https://provider.test/list.m3u?username=user&password=secret".to_owned(),
            },
            "integration-test",
        )
        .await
        .unwrap();
    let guide = sources
        .create(
            &NewSource {
                name: format!("Guide {suffix}"),
                kind: SourceKind::Xmltv,
                endpoint: "https://guide.test/guide.xml?token=secret".to_owned(),
            },
            "integration-test",
        )
        .await
        .unwrap();
    assert!(matches!(
        sources
            .create(
                &NewSource {
                    name: provider.source.name.clone(),
                    kind: SourceKind::Xtream,
                    endpoint: "https://other.test/player_api.php".to_owned(),
                },
                "integration-test",
            )
            .await,
        Err(PersistenceError::SourceConflict)
    ));

    assert!(jobs.cancel(provider.refresh_job.id).await.unwrap());
    assert!(jobs.cancel(guide.refresh_job.id).await.unwrap());
    sources
        .update_refresh_interval(provider.source.id, 60)
        .await
        .unwrap();
    sources
        .update_refresh_interval(guide.source.id, 120)
        .await
        .unwrap();
    assert!(matches!(
        sources
            .update_refresh_interval(provider.source.id, -1)
            .await,
        Err(PersistenceError::SourceConflict)
    ));
    sources
        .update_source(
            provider.source.id,
            &SourceUpdate {
                max_connections: Some(4),
                timezone: Some("America/Denver".to_owned()),
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
    sources
        .update_source(
            guide.source.id,
            &SourceUpdate {
                timezone: Some("America/New_York".to_owned()),
                ..SourceUpdate::default()
            },
        )
        .await
        .unwrap();
    let missing_id = uuid::Uuid::now_v7();
    assert!(matches!(
        sources
            .update_source(missing_id, &SourceUpdate::default())
            .await,
        Err(PersistenceError::SourceNotFound(id)) if id == missing_id
    ));
    assert!(matches!(
        sources.update_refresh_interval(missing_id, 60).await,
        Err(PersistenceError::SourceNotFound(id)) if id == missing_id
    ));
    assert!(matches!(
        sources.load_for_job(missing_id).await,
        Err(PersistenceError::SourceNotFound(id)) if id == missing_id
    ));

    let refreshed_at = Utc::now() - Duration::hours(1);
    sources
        .mark_source_refreshed(provider.source.id, refreshed_at)
        .await
        .unwrap();
    sources
        .mark_source_refreshed(guide.source.id, refreshed_at)
        .await
        .unwrap();
    let due = sources.list_due_sources().await.unwrap();
    assert!(due.iter().any(|source| {
        source.id == provider.source.id
            && source.kind == SourceKind::M3u
            && source.refresh_interval_seconds == 60
    }));
    assert!(due.iter().any(|source| {
        source.id == guide.source.id
            && source.kind == SourceKind::Xmltv
            && source.refresh_interval_seconds == 120
    }));

    let listed = sources.list().await.unwrap();
    let listed_provider = listed
        .iter()
        .find(|source| source.id == provider.source.id)
        .unwrap();
    assert_eq!(listed_provider.max_connections, 4);
    assert_eq!(listed_provider.timezone, "America/Denver");
    assert_eq!(
        sources
            .load_for_job(provider.source.id)
            .await
            .unwrap()
            .timezone,
        "America/Denver"
    );
    assert_eq!(
        sources
            .load_for_job(guide.source.id)
            .await
            .unwrap()
            .timezone,
        "America/New_York"
    );

    let queued_for_deleted_source = jobs
        .enqueue(&NewJob::immediate(
            "refresh-source",
            json!({"sourceId": provider.source.id}),
        ))
        .await
        .unwrap();
    sources
        .delete(provider.source.id, "integration-test")
        .await
        .unwrap();
    assert!(
        jobs.is_cancelled(queued_for_deleted_source.id)
            .await
            .unwrap()
    );
    assert!(matches!(
        sources
            .delete(provider.source.id, "integration-test")
            .await,
        Err(PersistenceError::SourceNotFound(id)) if id == provider.source.id
    ));
    sources
        .delete(guide.source.id, "integration-test")
        .await
        .unwrap();

    let mut retry = NewJob::immediate("retry-job", json!({"fixture": true}));
    retry.priority = i32::MAX;
    let retry = jobs.enqueue(&retry).await.unwrap();
    assert!(
        jobs.list_recent(0)
            .await
            .unwrap()
            .iter()
            .any(|job| job.id == retry.id)
    );
    let claimed = jobs.claim("coverage-worker").await.unwrap().unwrap();
    assert_eq!(claimed.id, retry.id);
    jobs.heartbeat(claimed.id, "coverage-worker", &json!({"step": 1}))
        .await
        .unwrap();
    jobs.fail(
        claimed.id,
        "coverage-worker",
        claimed.attempts,
        claimed.max_attempts,
        "temporary failure",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET available_at = now() WHERE id = $1")
        .bind(retry.id)
        .execute(&pool)
        .await
        .unwrap();
    let claimed = jobs.claim("coverage-worker").await.unwrap().unwrap();
    jobs.fail(
        claimed.id,
        "coverage-worker",
        claimed.max_attempts,
        claimed.max_attempts,
        "permanent failure",
    )
    .await
    .unwrap();
    assert!(
        jobs.list_failed_jobs(10)
            .await
            .unwrap()
            .iter()
            .any(|job| job.id == retry.id)
    );
    assert!(!jobs.cancel(retry.id).await.unwrap());
    assert!(!jobs.is_cancelled(retry.id).await.unwrap());
    assert!(matches!(
        jobs.is_cancelled(missing_id).await,
        Err(PersistenceError::JobNotFound(id)) if id == missing_id
    ));

    let mut stale = NewJob::immediate("stale-job", json!({}));
    stale.priority = i32::MAX;
    let stale = jobs.enqueue(&stale).await.unwrap();
    let claimed = jobs.claim("stale-worker").await.unwrap().unwrap();
    assert_eq!(claimed.id, stale.id);
    sqlx::query(
        "UPDATE jobs
         SET heartbeat_at = now() - interval '1 hour',
             locked_at = now() - interval '1 hour'
         WHERE id = $1",
    )
    .bind(stale.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(jobs.reap_stale_jobs(60).await.unwrap(), 1);
    assert!(jobs.cancel(stale.id).await.unwrap());
    assert!(jobs.claim("coverage-worker").await.unwrap().is_none());

    drop(jobs);
    drop(sources);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn list_due_sources_preserves_xtream_provider_kind() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let sources =
        SourceRepository::new(database.pool().clone(), MasterKey::from_bytes([32_u8; 32]));
    let source_id = uuid::Uuid::now_v7();

    sqlx::query(
        "INSERT INTO provider_accounts \
         (id, name, source_type, base_url_template, refresh_interval_seconds) \
         VALUES ($1, $2, 'xtream', 'https://provider.test/player_api.php', 60)",
    )
    .bind(source_id)
    .bind(format!("Due Xtream source {}", uuid::Uuid::now_v7()))
    .execute(database.pool())
    .await
    .unwrap();

    let due = sources.list_due_sources().await.unwrap();
    assert!(due.iter().any(|source| {
        source.id == source_id
            && source.kind == SourceKind::Xtream
            && source.refresh_interval_seconds == 60
            && source.last_refreshed_at.is_none()
    }));

    drop(sources);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn catalog_lineup_and_remaining_read_queries_work() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let account_id = uuid::Uuid::now_v7();
    let m3u_snapshot_id = uuid::Uuid::now_v7();
    let channel_id = uuid::Uuid::now_v7();
    let old_channel_id = uuid::Uuid::now_v7();
    let stream_id = uuid::Uuid::now_v7();
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let channel_name = format!("Lineup News {suffix}");
    let group_name = format!("Lineup Group {suffix}");

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Lineup account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, activated_at) VALUES ($1, $2, 'm3u', 'active', $3, 1, now())",
    )
    .bind(m3u_snapshot_id)
    .bind(account_id)
    .bind(format!("lineup-m3u-{suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    for (id, number, name, enabled) in [
        (
            channel_id,
            format!("lineup-1-{suffix}"),
            channel_name.clone(),
            true,
        ),
        (
            old_channel_id,
            format!("lineup-2-{suffix}"),
            format!("Old {channel_name}"),
            false,
        ),
    ] {
        sqlx::query(
            "INSERT INTO channels (id, channel_number, name, group_name, enabled, provider_account_id, canonical_key) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(number)
        .bind(name)
        .bind(&group_name)
        .bind(enabled)
        .bind(account_id)
        .bind(format!("channel-{id}"))
        .execute(&mut *transaction)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, 'lineup-stream', $4, '1', 'https://provider.test/lineup', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(stream_id)
    .bind(m3u_snapshot_id)
    .bind(account_id)
    .bind(&channel_name)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO channel_streams (channel_id, provider_stream_id, priority) VALUES ($1, $2, 0)",
    )
    .bind(channel_id)
    .bind(stream_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/lineup.xml', true)",
    )
    .bind(epg_source_id)
    .bind(format!("Lineup guide {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, epg_source_id, kind, status, checksum_sha256, byte_count, activated_at) VALUES ($1, $2, 'xmltv', 'active', $3, 1, now())",
    )
    .bind(epg_snapshot_id)
    .bind(epg_source_id)
    .bind(format!("lineup-xmltv-{suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id, display_names) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(epg_channel_id)
    .bind(epg_snapshot_id)
    .bind(epg_source_id)
    .bind(format!("lineup.xmltv.{suffix}"))
    .bind(json!([{"value": channel_name}]))
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    let source = catalog
        .channel_stream_source(channel_id)
        .await
        .unwrap()
        .expect("active stream source");
    assert_eq!(source.channel_id, channel_id);
    assert_eq!(source.stream_url, "https://provider.test/lineup");
    assert!(
        catalog
            .channel_playback_plan(uuid::Uuid::now_v7())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(catalog.list_enabled_channels().await.unwrap().len(), 1);
    assert_eq!(
        catalog
            .set_channel_enabled(old_channel_id, true)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        catalog.set_group_enabled(&group_name, false).await.unwrap(),
        2
    );
    assert_eq!(catalog.set_all_groups_enabled(true).await.unwrap(), 2);
    assert_eq!(catalog.list_groups().await.unwrap()[0].enabled_count, 2);

    let template = catalog
        .create_lineup_template(
            &format!("Lineup template {suffix}"),
            "lineup-package",
            "US",
            Some("coverage fixture"),
        )
        .await
        .unwrap();
    assert_eq!(catalog.list_lineup_templates().await.unwrap().len(), 1);
    assert_eq!(
        catalog
            .import_lineup(
                template.id,
                vec![(
                    group_name.clone(),
                    1,
                    vec![
                        (
                            channel_name.clone(),
                            "1".to_owned(),
                            vec!["News alias".to_owned()]
                        ),
                        ("No such channel".to_owned(), "99".to_owned(), vec![]),
                    ],
                )],
            )
            .await
            .unwrap(),
        2
    );
    let categories = catalog.list_lineup_categories(template.id).await.unwrap();
    assert_eq!(categories.len(), 1);
    assert_eq!(
        catalog
            .list_lineup_channels(categories[0].id)
            .await
            .unwrap()
            .len(),
        2
    );
    let applied = catalog.apply_lineup(template.id).await.unwrap();
    assert_eq!(applied.matched, 1);
    assert_eq!(applied.unmatched, 1);
    assert_eq!(applied.enabled, 1);
    assert_eq!(applied.disabled, 1);
    assert_eq!(
        catalog.delete_lineup_template(template.id).await.unwrap(),
        1
    );
    assert_eq!(
        catalog.delete_lineup_template(template.id).await.unwrap(),
        0
    );

    catalog
        .set_manual_epg_mapping(channel_id, epg_channel_id, "lineup-test")
        .await
        .unwrap();
    let mapping_page = catalog.list_epg_mappings(None, 0, -1).await.unwrap();
    assert_eq!(mapping_page.total, 1);
    assert_eq!(mapping_page.items[0].epg_channel_id, epg_channel_id);
    let applied_page = catalog
        .list_epg_mappings(Some("manual"), 5000, 0)
        .await
        .unwrap();
    assert_eq!(applied_page.items.len(), 1);
    let unmapped = catalog
        .list_unmapped_channels(Some("lineup"), 0, -1)
        .await
        .unwrap();
    assert!(unmapped.items.is_empty());
    catalog.remove_epg_mapping(channel_id).await.unwrap();
    let unmapped = catalog.list_unmapped_channels(None, 5000, 0).await.unwrap();
    assert!(unmapped.items.iter().any(|row| row.id == channel_id));
    let searched = catalog
        .search_epg_channels(&suffix.to_string(), 0)
        .await
        .unwrap();
    assert_eq!(searched.len(), 1);

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn operator_settings_persist_precedence_revisions_and_rollback() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let catalog = CatalogRepository::new(database.pool().clone());

    // Global override.
    let mut global = std::collections::BTreeMap::new();
    global.insert("media.ring.duration_seconds".to_owned(), json!(12));
    let stored = catalog
        .replace_operator_setting_scope("global", "", &global, None, "operator")
        .await
        .unwrap();
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.values["media.ring.duration_seconds"], json!(12));

    // Provider override (provider wins over global for the same key).
    let mut provider = std::collections::BTreeMap::new();
    provider.insert("media.ring.duration_seconds".to_owned(), json!(16));
    let stored = catalog
        .replace_operator_setting_scope("provider", "provider-a", &provider, None, "operator")
        .await
        .unwrap();
    assert_eq!(stored.revision, 1);

    // Group override (group wins over provider and global).
    let mut group = std::collections::BTreeMap::new();
    group.insert("events.inferred_duration_seconds".to_owned(), json!(9000));
    catalog
        .replace_operator_setting_scope("group", "sports", &group, None, "operator")
        .await
        .unwrap();

    // Load all overrides and verify the domain precedence map.
    let overrides: OperatorSettingOverrides =
        catalog.load_operator_setting_overrides().await.unwrap();
    assert_eq!(overrides.global["media.ring.duration_seconds"], json!(12));
    assert_eq!(
        overrides.providers["provider-a"]["media.ring.duration_seconds"],
        json!(16)
    );
    assert_eq!(
        overrides.groups["sports"]["events.inferred_duration_seconds"],
        json!(9000)
    );
    let domain = overrides.to_domain("provider-a", "sports");
    assert_eq!(domain.global["media.ring.duration_seconds"], json!(12));
    assert_eq!(domain.provider["media.ring.duration_seconds"], json!(16));
    assert_eq!(
        domain.group["events.inferred_duration_seconds"],
        json!(9000)
    );

    // Stale expected revision returns a conflict and leaves data unchanged.
    let mut next = std::collections::BTreeMap::new();
    next.insert("media.ring.duration_seconds".to_owned(), json!(20));
    let conflict = catalog
        .replace_operator_setting_scope("global", "", &next, Some(99), "operator")
        .await;
    assert!(matches!(
        conflict,
        Err(PersistenceError::SettingRevisionConflict {
            expected: 99,
            actual: 1
        })
    ));
    let still = catalog
        .load_operator_setting_scope("global", "")
        .await
        .unwrap();
    assert_eq!(still.revision, 1);
    assert_eq!(still.values["media.ring.duration_seconds"], json!(12));

    // Fresh expected revision succeeds and bumps to 2.
    let stored = catalog
        .replace_operator_setting_scope("global", "", &next, Some(1), "operator")
        .await
        .unwrap();
    assert_eq!(stored.revision, 2);
    assert_eq!(stored.values["media.ring.duration_seconds"], json!(20));

    // Revision history is recorded newest first.
    let revisions = catalog
        .list_operator_setting_revisions("global", "", 500)
        .await
        .unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].revision, 2);
    assert_eq!(revisions[1].revision, 1);
    let before: Value = revisions[0].before_value.clone().unwrap();
    assert_eq!(
        before["overrides"]["media.ring.duration_seconds"],
        json!(12)
    );
    assert_eq!(
        revisions[0].after_value["media.ring.duration_seconds"],
        json!(20)
    );

    let newest = catalog
        .list_operator_setting_revisions("global", "", 1)
        .await
        .unwrap();
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].revision, 2);

    // Rollback to revision 1 restores the earlier value and bumps to 3.
    let restored = catalog
        .rollback_operator_setting_scope("global", "", 1, "operator")
        .await
        .unwrap();
    assert_eq!(restored.revision, 3);
    assert_eq!(restored.values["media.ring.duration_seconds"], json!(12));

    // Rollback to a missing revision returns not found.
    let missing = catalog
        .rollback_operator_setting_scope("global", "", 99, "operator")
        .await;
    assert!(matches!(
        missing,
        Err(PersistenceError::SettingRevisionNotFound { target: 99, .. })
    ));

    drop(catalog);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn operator_settings_scope_revision_survives_empty_override_set() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let catalog = CatalogRepository::new(database.pool().clone());

    // Seed one override so the scope has a recorded revision.
    let mut first = std::collections::BTreeMap::new();
    first.insert("media.ring.duration_seconds".to_owned(), json!(12));
    let stored = catalog
        .replace_operator_setting_scope("global", "", &first, None, "operator")
        .await
        .unwrap();
    assert_eq!(stored.revision, 1);

    // Clear the scope. The revisions table keeps revision 2 even though the
    // override rows are deleted.
    let cleared = catalog
        .replace_operator_setting_scope(
            "global",
            "",
            &std::collections::BTreeMap::new(),
            Some(1),
            "operator",
        )
        .await
        .unwrap();
    assert_eq!(cleared.revision, 2);
    assert!(cleared.values.is_empty());

    // The load path must read revision 2 from revisions, not 0 from the empty
    // override set. The returned ETag must match the write.
    let loaded = catalog
        .load_operator_setting_scope("global", "")
        .await
        .unwrap();
    assert_eq!(loaded.revision, 2);
    assert!(loaded.values.is_empty());

    // A fresh If-Match write with the loaded revision must succeed and bump
    // the revision to 3.
    let mut next = std::collections::BTreeMap::new();
    next.insert("media.ring.duration_seconds".to_owned(), json!(30));
    let stored = catalog
        .replace_operator_setting_scope("global", "", &next, Some(2), "operator")
        .await
        .unwrap();
    assert_eq!(stored.revision, 3);
    assert_eq!(stored.values["media.ring.duration_seconds"], json!(30));

    drop(catalog);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_epg_mappings_are_stable_across_input_permutations() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    // Seed one provider account with one channel whose normalized name matches
    // two EPG channels. The ambiguous match must populate review_candidates
    // deterministically regardless of the EPG channel insertion order.
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Permutation account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, 'sports', 'Denver Sports Network', NULL, '4.1', 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(snapshot_id)
    .bind(account_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();

    // Seed an active XMLTV snapshot with two EPG channels that share the same
    // normalized display name. Insert them in one order and capture the
    // review candidate set.
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)")
        .bind(epg_source_id)
        .bind(format!("Permutation guide {suffix}"))
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO source_snapshots (id, epg_source_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'xmltv', 'active', $3, 1, 2)")
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .bind(epg_snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
    let epg_a = uuid::Uuid::now_v7();
    let epg_b = uuid::Uuid::now_v7();
    for (id, xmltv) in [(epg_a, "denver-sports-a"), (epg_b, "denver-sports-b")] {
        sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id, display_names) VALUES ($1, $2, $3, $4, '[{\"value\":\"Denver Sports Network\"}]'::jsonb)")
            .bind(id)
            .bind(epg_snapshot_id)
            .bind(epg_source_id)
            .bind(xmltv)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    catalog.reconcile_epg_mappings().await.unwrap();
    let first_ids: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT epg_channel_id FROM review_candidates ORDER BY epg_channel_id")
            .fetch_all(&pool)
            .await
            .unwrap();

    // Clear the review candidates and EPG channels, then re-insert the same
    // two EPG channels in the reverse order. The review candidate set must be
    // identical because the SQL aggregation orders by epg_channel_id.
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM review_candidates")
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM channel_epg_mappings USING channels WHERE channel_epg_mappings.channel_id = channels.id AND channels.provider_account_id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM epg_channels WHERE source_snapshot_id = $1")
        .bind(epg_snapshot_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    for (id, xmltv) in [(epg_b, "denver-sports-b"), (epg_a, "denver-sports-a")] {
        sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id, display_names) VALUES ($1, $2, $3, $4, '[{\"value\":\"Denver Sports Network\"}]'::jsonb)")
            .bind(id)
            .bind(epg_snapshot_id)
            .bind(epg_source_id)
            .bind(xmltv)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    catalog.reconcile_epg_mappings().await.unwrap();
    let second_ids: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT epg_channel_id FROM review_candidates ORDER BY epg_channel_id")
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(
        first_ids, second_ids,
        "review candidates must be stable across input permutations"
    );
    assert_eq!(first_ids.len(), 2);

    let mut transaction = pool.begin().await.unwrap();
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

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconcile_epg_mappings_preserve_manual_bindings() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    // Seed one provider account with one stream that has no tvg-id so the
    // automatic EPG reconcile cannot match it by tvg-id.
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Manual binding account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, 'manual', 'Manual Channel', NULL, '8.1', 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(snapshot_id)
    .bind(account_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();

    // Seed an active XMLTV snapshot with one EPG channel that does not match
    // the channel name, so the automatic reconcile leaves the channel unmapped.
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)")
        .bind(epg_source_id)
        .bind(format!("Manual binding guide {suffix}"))
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
    sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id, display_names) VALUES ($1, $2, $3, 'manual.target', '[{\"value\":\"Target EPG Channel\"}]'::jsonb)")
        .bind(epg_channel_id)
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    // Resolve the canonical channel id and set a manual binding.
    let channel_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic' LIMIT 1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    catalog
        .set_manual_epg_mapping(channel_id, epg_channel_id, "integration-test")
        .await
        .unwrap();

    // Run the automatic EPG reconcile. The manual binding must survive.
    let stats = catalog.reconcile_epg_mappings().await.unwrap();
    let mappings = catalog
        .list_epg_mappings(Some("manual"), 100, 0)
        .await
        .unwrap();
    let manual = mappings
        .items
        .iter()
        .find(|row| row.channel_id == channel_id)
        .expect("manual mapping must survive reconciliation");
    assert_eq!(manual.review_status, "manual");
    assert_eq!(manual.epg_channel_id, epg_channel_id);
    // The automatic reconcile must not have applied a competing mapping.
    assert_eq!(stats.mappings_applied, 0);

    let mut transaction = pool.begin().await.unwrap();
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

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconciliation_rollback_restores_channels_streams_and_epg_mappings() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    // Seed one provider account with one supported stream.
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let stream_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Rollback account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, attributes, directives, supported) VALUES ($1, $2, $3, 'news', 'News HD', 'news.tvg', '7.1', 'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
    )
    .bind(stream_id)
    .bind(snapshot_id)
    .bind(account_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    // First reconciliation records revision 1 and creates one channel with
    // one stream link.
    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();
    let channel_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic' LIMIT 1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    // Seed an EPG channel that matches by tvg-id and run the global EPG
    // reconcile so the channel gets an automatic EPG mapping.
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO epg_sources (id, name, url_template, enabled) VALUES ($1, $2, 'https://guide.test/g.xml', true)")
        .bind(epg_source_id)
        .bind(format!("Rollback guide {suffix}"))
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
    sqlx::query("INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id) VALUES ($1, $2, $3, 'news.tvg')")
        .bind(epg_channel_id)
        .bind(epg_snapshot_id)
        .bind(epg_source_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    catalog.reconcile_epg_mappings().await.unwrap();

    // Run a second per-account reconciliation so the EPG mapping is captured
    // in a revision snapshot. Revision 1 held only the channel and stream
    // link because the global EPG reconcile ran after it. Revision 2 captures
    // the channel, stream link, and EPG mapping together.
    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();

    let link_count_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_streams cs JOIN channels c ON c.id = cs.channel_id WHERE c.provider_account_id = $1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(link_count_before, 1);
    let mapping_count_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_epg_mappings cem JOIN channels c ON c.id = cem.channel_id WHERE c.provider_account_id = $1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mapping_count_before, 1);

    // Revision 2 holds the full state. Roll back to it after a destructive
    // third reconciliation removes the channel.
    let revisions_before = catalog
        .list_reconciliation_revisions(account_id)
        .await
        .unwrap();
    assert_eq!(revisions_before.len(), 2);
    let target_revision = revisions_before[0].revision;
    assert_eq!(target_revision, 2);

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("UPDATE provider_streams SET supported = false WHERE id = $1")
        .bind(stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();

    // The third reconciliation removed the channel, stream link, and EPG
    // mapping.
    let channel_count_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic'",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(channel_count_after, 0);

    // Rollback to revision 2 restores the channel, stream link, and EPG
    // mapping captured at that revision.
    let stats = catalog
        .rollback_reconciliation(account_id, target_revision, "integration-test")
        .await
        .unwrap();
    assert_eq!(stats.target_revision, target_revision);
    assert_eq!(stats.channels_restored, 1);
    assert_eq!(stats.stream_links_restored, 1);
    assert_eq!(stats.epg_mappings_restored, 1);

    let restored_channel_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic'",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored_channel_count, 1);
    let restored_channel_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM channels WHERE provider_account_id = $1 AND managed_by = 'automatic' LIMIT 1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored_channel_id, channel_id, "channel id must be stable");

    let restored_link_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_streams cs JOIN channels c ON c.id = cs.channel_id WHERE c.provider_account_id = $1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored_link_count, 1);

    let restored_mapping_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_epg_mappings cem JOIN channels c ON c.id = cem.channel_id WHERE c.provider_account_id = $1",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored_mapping_count, 1);

    // The rollback revision must capture the pre-rollback state in
    // `before_value`. The previous implementation captured both snapshots
    // after restoration, which made the audit transition non-reversible.
    let rollback_revision = catalog
        .list_reconciliation_revisions(account_id)
        .await
        .unwrap()
        .into_iter()
        .find(|revision| revision.actor == "integration-test")
        .expect("rollback revision is recorded");
    assert_eq!(rollback_revision.revision, 4);
    assert_eq!(
        rollback_revision
            .before_value
            .as_ref()
            .and_then(|value| value["channels"].as_array())
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(
        rollback_revision.after_value["channels"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    // Rollback to a missing revision returns the not-found error.
    let missing = catalog
        .rollback_reconciliation(account_id, 99, "integration-test")
        .await;
    assert!(matches!(
        missing,
        Err(PersistenceError::ReconciliationRevisionNotFound { target: 99, .. })
    ));

    let mut transaction = pool.begin().await.unwrap();
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

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reconciliation_rollback_preserves_channel_dependents() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let stream_id = uuid::Uuid::now_v7();
    let extra_channel_id = uuid::Uuid::now_v7();
    let output_profile_id = uuid::Uuid::now_v7();
    let user_id = uuid::Uuid::now_v7();
    let recording_rule_id = uuid::Uuid::now_v7();
    let recording_id = uuid::Uuid::now_v7();
    let event_template_id = uuid::Uuid::now_v7();
    let event_channel_id = uuid::Uuid::now_v7();
    let generated_id = uuid::Uuid::now_v7();
    let epg_source_id = uuid::Uuid::now_v7();
    let epg_snapshot_id = uuid::Uuid::now_v7();
    let epg_channel_id = uuid::Uuid::now_v7();
    let review_id = uuid::Uuid::now_v7();

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template) VALUES ($1, $2, 'm3u', 'https://provider.test/')",
    )
    .bind(account_id)
    .bind(format!("Dependent rollback account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, tvg_id, channel_number, url_template, supported) VALUES ($1, $2, $3, 'target', 'Target', 'target.tvg', '101', 'https://provider.test/stream', true)",
    )
    .bind(stream_id)
    .bind(snapshot_id)
    .bind(account_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_sources (id, name, url_template) VALUES ($1, $2, 'https://guide.test/guide.xml')",
    )
    .bind(epg_source_id)
    .bind(format!("Dependent guide {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_snapshots (id, epg_source_id, kind, status, checksum_sha256, byte_count, record_count) VALUES ($1, $2, 'xmltv', 'active', $3, 1, 1)",
    )
    .bind(epg_snapshot_id)
    .bind(epg_source_id)
    .bind(epg_snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO epg_channels (id, source_snapshot_id, epg_source_id, xmltv_id) VALUES ($1, $2, $3, 'dependent.epg')",
    )
    .bind(epg_channel_id)
    .bind(epg_snapshot_id)
    .bind(epg_source_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    catalog
        .reconcile_provider_account(account_id)
        .await
        .unwrap();
    let target_revision = catalog
        .list_reconciliation_revisions(account_id)
        .await
        .unwrap()[0]
        .revision;

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO channels (id, channel_number, name, group_name, provider_account_id, canonical_key, managed_by) VALUES ($1, '102', 'Obsolete', 'Test', $2, 'obsolete.tvg', 'automatic')",
    )
    .bind(extra_channel_id)
    .bind(account_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO output_profiles (id, name, token_hash, tuner_count) VALUES ($1, $2, $3, 1)",
    )
    .bind(output_profile_id)
    .bind(format!("Dependent output {suffix}"))
    .bind(vec![8_u8; 32])
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO output_profile_channels (output_profile_id, channel_id, position) VALUES ($1, $2, 0)",
    )
    .bind(output_profile_id)
    .bind(extra_channel_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash) VALUES ($1, $2, 'Dependent User', 'hash')",
    )
    .bind(user_id)
    .bind(format!("dependent-{suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_channel_grants (user_id, channel_id) VALUES ($1, $2)")
        .bind(user_id)
        .bind(extra_channel_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO recording_rules (id, name, channel_id) VALUES ($1, 'Dependent rule', $2)",
    )
    .bind(recording_rule_id)
    .bind(extra_channel_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO recordings (id, rule_id, channel_id, title, starts_at, ends_at) VALUES ($1, $2, $3, 'Dependent recording', now(), now() + interval '1 hour')",
    )
    .bind(recording_id)
    .bind(recording_rule_id)
    .bind(extra_channel_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO channel_stream_profiles (channel_id, stream_profile_id) VALUES ($1, '22222222-0000-0000-0000-000000000001')",
    )
    .bind(extra_channel_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO event_templates (id, name, display_name, match_regex, channel_name_format, group_name) VALUES ($1, $2, 'Dependent event', '.*', '{event}', 'Test')",
    )
    .bind(event_template_id)
    .bind(format!("dependent-event-{suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO event_channels (id, template_id, channel_id, slot_number) VALUES ($1, $2, $3, 1)",
    )
    .bind(event_channel_id)
    .bind(event_template_id)
    .bind(extra_channel_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO generated_programmes (id, event_channel_id, channel_id, template_id, rule_id, rule_name, source_title, kind, starts_at, stops_at, title) VALUES ($1, $2, $3, $4, $5, 'Dependent rule', 'Dependent event', 'event', now(), now() + interval '30 minutes', 'Dependent event')",
    )
    .bind(generated_id)
    .bind(event_channel_id)
    .bind(extra_channel_id)
    .bind(event_template_id)
    .bind(uuid::Uuid::now_v7())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO review_candidates (id, channel_id, epg_channel_id, method, confidence) VALUES ($1, $2, $3, 'fuzzy', 0.5)",
    )
    .bind(review_id)
    .bind(extra_channel_id)
    .bind(epg_channel_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    let stats = catalog
        .rollback_reconciliation(account_id, target_revision, "dependent-test")
        .await
        .unwrap();
    assert_eq!(stats.channels_removed, 0);

    for (table, column) in [
        ("output_profile_channels", "channel_id"),
        ("user_channel_grants", "channel_id"),
        ("recording_rules", "channel_id"),
        ("recordings", "channel_id"),
        ("channel_stream_profiles", "channel_id"),
        ("generated_programmes", "channel_id"),
        ("event_channels", "channel_id"),
        ("review_candidates", "channel_id"),
    ] {
        let query = format!("SELECT count(*) FROM {table} WHERE {column} = $1");
        let count: i64 = sqlx::query_scalar(&query)
            .bind(extra_channel_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "dependent row in {table} must survive rollback");
    }
    let detached: (bool, String, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT enabled, managed_by, provider_account_id FROM channels WHERE id = $1",
    )
    .bind(extra_channel_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(detached, (false, "manual".to_owned(), None));

    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

/// Inserts one active Xtream live snapshot with two supported numeric streams.
async fn seed_xtream_short_epg_streams(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    max_connections: i32,
) -> uuid::Uuid {
    sqlx::query("UPDATE provider_accounts SET max_connections = $2 WHERE id = $1")
        .bind(account_id)
        .bind(max_connections)
        .execute(pool)
        .await
        .unwrap();

    // Supersede any prior active xtream snapshot so the new active row does
    // not violate the one-active-snapshot-per-kind index.
    sqlx::query(
        "UPDATE source_snapshots SET status = 'superseded', activated_at = coalesce(activated_at, now()) \
         WHERE provider_account_id = $1 AND kind = 'xtream' AND status = 'active'",
    )
    .bind(account_id)
    .execute(pool)
    .await
    .unwrap();

    let snapshot_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO source_snapshots \
         (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
         VALUES ($1, $2, 'xtream', 'active', $3, 1, 2)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(pool)
    .await
    .unwrap();

    for (stable_key, stream_id, tvg_id, channel_number) in [
        ("sports-7", "7", "worker-epg-7", "7"),
        ("sports-8", "8", "worker-epg-8", "8"),
    ] {
        sqlx::query(
            "INSERT INTO provider_streams \
             (id, snapshot_id, provider_account_id, stable_key, provider_stream_id, \
              name, tvg_id, channel_number, url_template, attributes, directives, supported) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
                     'https://provider.test/stream', '{}'::jsonb, '[]'::jsonb, true)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(snapshot_id)
        .bind(account_id)
        .bind(stable_key)
        .bind(stream_id)
        .bind(format!("Worker Sports {stream_id}"))
        .bind(tvg_id)
        .bind(channel_number)
        .execute(pool)
        .await
        .unwrap();
    }
    snapshot_id
}

#[tokio::test]
async fn enqueue_xtream_short_epg_deduplicates_active_jobs() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([41_u8; 32]));
    let jobs = JobRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();

    let created = sources
        .create(
            &NewSource {
                name: format!("Xtream short EPG dedup {suffix}"),
                kind: SourceKind::Xtream,
                endpoint: "https://provider.test/player_api.php?username=user&password=secret"
                    .to_owned(),
            },
            "integration-test",
        )
        .await
        .unwrap();
    let source_id = created.source.id;
    jobs.cancel(created.refresh_job.id).await.unwrap();

    // The first enqueue creates one queued job.
    let first = jobs
        .enqueue_xtream_short_epg(source_id)
        .await
        .unwrap()
        .expect("first enqueue creates a job");
    assert_eq!(first.kind, "refresh-xtream-short-epg");
    assert_eq!(first.status, "queued");

    // A second enqueue while the first job is queued returns None and does
    // not insert a duplicate row.
    let second = jobs.enqueue_xtream_short_epg(source_id).await.unwrap();
    assert!(second.is_none(), "duplicate enqueue must be suppressed");
    let queued_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs \
         WHERE kind = 'refresh-xtream-short-epg' \
           AND payload->>'sourceId' = $1 \
           AND status = 'queued'",
    )
    .bind(source_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued_count, 1, "only one queued short EPG job may exist");

    // When the queued job transitions to running, the dedup guard still
    // suppresses a new enqueue because running jobs are also active.
    sqlx::query(
        "UPDATE jobs SET status = 'running', locked_by = 'worker', locked_at = now() \
         WHERE id = $1",
    )
    .bind(first.id)
    .execute(&pool)
    .await
    .unwrap();
    let while_running = jobs.enqueue_xtream_short_epg(source_id).await.unwrap();
    assert!(
        while_running.is_none(),
        "enqueue while running must be suppressed"
    );

    // When the running job completes, the dedup guard releases and a new
    // enqueue creates a fresh queued job.
    sqlx::query("UPDATE jobs SET status = 'succeeded', completed_at = now() WHERE id = $1")
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
    let next = jobs
        .enqueue_xtream_short_epg(source_id)
        .await
        .unwrap()
        .expect("enqueue after completion creates a job");
    assert_ne!(next.id, first.id);
    let queued_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs \
         WHERE kind = 'refresh-xtream-short-epg' \
           AND payload->>'sourceId' = $1 \
           AND status = 'queued'",
    )
    .bind(source_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        queued_count, 1,
        "a fresh queued job replaces the completed one"
    );

    sources.delete(source_id, "integration-test").await.unwrap();
    drop(jobs);
    drop(sources);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn list_xtream_short_epg_streams_applies_bounded_capacity() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([42_u8; 32]));
    let suffix = uuid::Uuid::now_v7();

    let created = sources
        .create(
            &NewSource {
                name: format!("Xtream short EPG bounds {suffix}"),
                kind: SourceKind::Xtream,
                endpoint: "https://provider.test/player_api.php?username=user&password=secret"
                    .to_owned(),
            },
            "integration-test",
        )
        .await
        .unwrap();
    let source_id = created.source.id;
    // Cancel the auto-enqueued refresh job so it does not interfere.
    let auto_jobs: Vec<uuid::Uuid> = sqlx::query_as(
        "SELECT id FROM jobs \
         WHERE kind = 'refresh-source' AND payload->>'sourceId' = $1",
    )
    .bind(source_id.to_string())
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(id,): (uuid::Uuid,)| id)
    .collect();
    for id in auto_jobs {
        let _ = jobs_cancel(&pool, id).await;
    }

    // Seed two supported numeric streams under an account capacity of one.
    seed_xtream_short_epg_streams(&pool, source_id, 1).await;

    // The requested limit is clamped to the effective capacity of one, so
    // only the first stream (ordered by channel number) is selected.
    let selected = sources
        .list_xtream_short_epg_streams(source_id, 16)
        .await
        .unwrap();
    assert_eq!(
        selected.len(),
        1,
        "selection must obey the account capacity bound"
    );
    assert_eq!(selected[0].stream_id, 7);
    assert_eq!(selected[0].channel_id, "worker-epg-7");

    // A requested limit below the capacity is honored and clamped to the
    // minimum of one, so the selection still returns one stream.
    let small = sources
        .list_xtream_short_epg_streams(source_id, 1)
        .await
        .unwrap();
    assert_eq!(small.len(), 1);
    assert_eq!(small[0].stream_id, 7);

    // When the account capacity grows to two, both streams are selected up
    // to the requested limit.
    seed_xtream_short_epg_streams(&pool, source_id, 2).await;
    let both = sources
        .list_xtream_short_epg_streams(source_id, 16)
        .await
        .unwrap();
    assert_eq!(
        both.len(),
        2,
        "selection must return both streams when capacity allows"
    );
    assert_eq!(both[0].stream_id, 7);
    assert_eq!(both[1].stream_id, 8);
    // The channel_id falls back to the provider_stream_id when tvg_id is
    // empty, so the helper must keep the tvg_id here.
    assert_eq!(both[0].channel_id, "worker-epg-7");
    assert_eq!(both[1].channel_id, "worker-epg-8");

    // The selection excludes unsupported streams and non-numeric stream ids.
    sqlx::query(
        "UPDATE provider_streams SET supported = false \
         WHERE provider_account_id = $1 AND provider_stream_id = '8'",
    )
    .bind(source_id)
    .execute(&pool)
    .await
    .unwrap();
    let after_disable = sources
        .list_xtream_short_epg_streams(source_id, 16)
        .await
        .unwrap();
    assert_eq!(
        after_disable.len(),
        1,
        "unsupported streams must be excluded"
    );
    assert_eq!(after_disable[0].stream_id, 7);

    sources.delete(source_id, "integration-test").await.unwrap();
    drop(sources);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

async fn jobs_cancel(pool: &sqlx::PgPool, job_id: uuid::Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE jobs SET status = 'cancelled', completed_at = now() \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Inserts one supported provider stream and returns its ID.
async fn insert_probe_stream(
    pool: &sqlx::PgPool,
    suffix: uuid::Uuid,
) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let mut transaction = pool.begin().await.unwrap();
    let account_id = uuid::Uuid::now_v7();
    let snapshot_id = uuid::Uuid::now_v7();
    let stream_id = uuid::Uuid::now_v7();

    sqlx::query(
        "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) \
         VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
    )
    .bind(account_id)
    .bind(format!("Dedup account {suffix}"))
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
         VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
    )
    .bind(snapshot_id)
    .bind(account_id)
    .bind(snapshot_id.to_string())
    .execute(&mut *transaction)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, supported) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, true)",
    )
    .bind(stream_id)
    .bind(snapshot_id)
    .bind(account_id)
    .bind(format!("dedup-key-{suffix}"))
    .bind("Dedup Stream")
    .bind("news")
    .bind("https://provider.test/stream.ts")
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    (account_id, snapshot_id, stream_id)
}

async fn delete_probe_stream(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    snapshot_id: uuid::Uuid,
    stream_id: uuid::Uuid,
) {
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
        .bind(stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_streams WHERE id = $1")
        .bind(stream_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn try_mark_stream_checking_deduplicates_concurrent_probes() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let (account_id, snapshot_id, stream_id) = insert_probe_stream(&pool, suffix).await;

    // A fresh stream is `unknown`. The first claim must transition it to
    // `checking` and return true.
    let first = catalog.try_mark_stream_checking(stream_id).await.unwrap();
    assert!(first, "first claim must succeed for an unknown stream");
    let status: String =
        sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "checking");

    // Admission failure must release only the current checking claim.
    catalog
        .release_stream_checking_claim(stream_id)
        .await
        .unwrap();
    let released_status: String =
        sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(released_status, "unknown");
    assert!(catalog.try_mark_stream_checking(stream_id).await.unwrap());

    // A second concurrent claim must return false and leave the stream in
    // `checking` so a second worker does not probe the same stream.
    let second = catalog.try_mark_stream_checking(stream_id).await.unwrap();
    assert!(!second, "second claim must be suppressed while checking");

    // An `alive` stream must not be claimed by a probe.
    sqlx::query("UPDATE provider_streams SET health_status = 'alive' WHERE id = $1")
        .bind(stream_id)
        .execute(&pool)
        .await
        .unwrap();
    let alive_claim = catalog.try_mark_stream_checking(stream_id).await.unwrap();
    assert!(!alive_claim, "an alive stream must not be claimed");

    delete_probe_stream(&pool, account_id, snapshot_id, stream_id).await;
    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn enqueue_health_probe_if_idle_deduplicates_active_jobs() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let jobs = JobRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let (account_id, snapshot_id, stream_id) = insert_probe_stream(&pool, suffix).await;

    // The first enqueue creates one queued low-priority health-probe job.
    let first = jobs
        .enqueue_health_probe_if_idle(stream_id, -1, 2)
        .await
        .unwrap()
        .expect("first enqueue creates a job");
    assert_eq!(first.kind, "health-probe");
    assert_eq!(first.priority, -1);
    assert_eq!(first.status, "queued");

    // A second enqueue while the first is queued is suppressed.
    let second = jobs
        .enqueue_health_probe_if_idle(stream_id, -1, 2)
        .await
        .unwrap();
    assert!(
        second.is_none(),
        "duplicate queued enqueue must be suppressed"
    );
    let queued_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs \
         WHERE kind = 'health-probe' \
           AND payload->>'providerStreamId' = $1 \
           AND status = 'queued'",
    )
    .bind(stream_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        queued_count, 1,
        "only one queued health-probe job may exist"
    );

    // While the job is running the dedup guard still suppresses a new enqueue.
    sqlx::query(
        "UPDATE jobs SET status = 'running', locked_by = 'worker', locked_at = now() WHERE id = $1",
    )
    .bind(first.id)
    .execute(&pool)
    .await
    .unwrap();
    let while_running = jobs
        .enqueue_health_probe_if_idle(stream_id, -1, 2)
        .await
        .unwrap();
    assert!(
        while_running.is_none(),
        "enqueue while running must be suppressed"
    );

    // After the running job completes, a fresh enqueue creates a new job.
    sqlx::query("UPDATE jobs SET status = 'succeeded', completed_at = now() WHERE id = $1")
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
    let next = jobs
        .enqueue_health_probe_if_idle(stream_id, -1, 2)
        .await
        .unwrap()
        .expect("enqueue after completion creates a job");
    assert_ne!(next.id, first.id);

    jobs_cancel(&pool, first.id).await.ok();
    jobs_cancel(&pool, next.id).await.ok();
    delete_probe_stream(&pool, account_id, snapshot_id, stream_id).await;
    drop(jobs);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}

#[tokio::test]
async fn skipped_probe_does_not_persist_an_invalid_health_check_status() {
    let Some(database_url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping PostgreSQL integration test");
        return;
    };
    let (admin, database, schema) = isolated_database(&database_url).await;
    let pool = database.pool().clone();
    let catalog = CatalogRepository::new(pool.clone());
    let suffix = uuid::Uuid::now_v7();
    let (account_id, snapshot_id, stream_id) = insert_probe_stream(&pool, suffix).await;

    // Seed a known-good alive health check so a skipped probe must not overwrite
    // it with an invalid `unknown` status.
    catalog
        .update_stream_health(&StreamHealthUpdate {
            provider_stream_id: stream_id,
            status: "alive".to_owned(),
            error: None,
            video_codec: Some("h264".to_owned()),
            video_resolution: None,
            video_width: Some(1920),
            video_height: Some(1080),
            video_fps: Some(30.0),
            audio_codec: Some("mp2".to_owned()),
            audio_channels: None,
            audio_sample_rate: None,
            bitrate_kbps: Some(4000),
            check_duration_ms: Some(500),
        })
        .await
        .unwrap();

    // A skipped probe persists no update. The caller (gateway) is responsible
    // for not calling this method on a skipped outcome; this test verifies the
    // known-good row is untouched when no update is written.
    let status: String =
        sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "alive");
    let checks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stream_health_checks WHERE provider_stream_id = $1",
    )
    .bind(stream_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(checks, 1, "only the seeded alive check row exists");

    delete_probe_stream(&pool, account_id, snapshot_id, stream_id).await;
    drop(catalog);
    drop(pool);
    drop(database);
    drop_isolated_schema(&admin, &schema).await;
}
