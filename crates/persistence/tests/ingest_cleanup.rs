use iptv_persistence::{CatalogRepository, Database, IngestCleanupRepository, PersistenceError};
use serde_json::json;
use uuid::Uuid;

fn database_url() -> Option<String> {
    std::env::var("IPTV_TEST_DATABASE_URL").ok()
}

async fn isolated_database(url: &str) -> (Database, Database, String) {
    let admin = Database::connect(url, 2).await.unwrap();
    let schema = format!("iptv_cleanup_{}", Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .unwrap();
    let mut parsed = url::Url::parse(url).unwrap();
    parsed
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema},public"));
    let database = Database::connect(parsed.as_str(), 4).await.unwrap();
    database.migrate().await.unwrap();
    (admin, database, schema)
}

async fn teardown(admin: &Database, schema: &str) {
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .unwrap();
}

async fn provider(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO provider_accounts (id,name,source_type,base_url_template) VALUES ($1,$2,'m3u','https://example.invalid')")
        .bind(id).bind(format!("cleanup-{id}")).execute(pool).await.unwrap();
    id
}

async fn snapshot(pool: &sqlx::PgPool, account: Uuid, status: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO source_snapshots (id,provider_account_id,kind,status,checksum_sha256,byte_count,fetched_at,staged_at) VALUES ($1,$2,'m3u',$3,$4,1,now()-interval '2 days',now()-interval '2 days')")
        .bind(id).bind(account).bind(status).bind(id.to_string()).execute(pool).await.unwrap();
    id
}

#[tokio::test]
async fn cleanup_retains_active_and_protected_snapshots_and_removes_orphan_runs() {
    let Some(url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping");
        return;
    };
    let (admin, db, schema) = isolated_database(&url).await;
    let account = provider(db.pool()).await;
    let active = snapshot(db.pool(), account, "active").await;
    let removable_account = provider(db.pool()).await;
    let removable = snapshot(db.pool(), removable_account, "superseded").await;
    let outbox_snapshot = snapshot(db.pool(), account, "superseded").await;
    let job_snapshot = snapshot(db.pool(), account, "superseded").await;
    let parent = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO jobs (id,kind,status,payload) VALUES ($1,'refresh-source','queued',$2)",
    )
    .bind(parent)
    .bind(json!({"sourceId": account}))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO job_outbox (id,parent_job_id,kind,payload,dedup_key) VALUES ($1,$2,'prepare-provider-reconciliation',$3,$4)")
        .bind(Uuid::now_v7()).bind(parent).bind(json!({"snapshotId": outbox_snapshot})).bind(Uuid::now_v7().to_string()).execute(db.pool()).await.unwrap();
    let run = Uuid::now_v7();
    sqlx::query("INSERT INTO provider_reconciliation_runs (id,provider_account_id,source_snapshot_id,status,partition_count,updated_at) VALUES ($1,$2,$3,'processing',1,now()-interval '2 days')")
        .bind(run).bind(account).bind(removable).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO provider_reconciliation_candidates (run_id,canonical_key,name) VALUES ($1,'orphan','orphan')")
        .bind(run).execute(db.pool()).await.unwrap();
    let cleanup = IngestCleanupRepository::new(db.pool().clone());
    let stats = cleanup.cleanup_batch(86_400, 1).await.unwrap();
    assert_eq!(stats.reconciliation_runs, 1);
    assert_eq!(stats.snapshots, 1);
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM source_snapshots WHERE id=$1)")
            .bind(active)
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM source_snapshots WHERE id=$1)"
        )
        .bind(removable)
        .fetch_one(db.pool())
        .await
        .unwrap()
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM source_snapshots WHERE id=$1)")
            .bind(outbox_snapshot)
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM source_snapshots WHERE id=$1)")
            .bind(job_snapshot)
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM provider_reconciliation_runs WHERE id=$1)"
        )
        .bind(run)
        .fetch_one(db.pool())
        .await
        .unwrap()
    );
    teardown(&admin, &schema).await;
}

#[tokio::test]
async fn cleanup_is_idempotent_and_concurrent_calls_do_not_duplicate_work() {
    let Some(url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping");
        return;
    };
    let (admin, db, schema) = isolated_database(&url).await;
    let account = provider(db.pool()).await;
    let _ = snapshot(db.pool(), account, "superseded").await;
    let cleanup = IngestCleanupRepository::new(db.pool().clone());
    let (a, b) = tokio::join!(
        cleanup.cleanup_batch(86_400, 10),
        cleanup.cleanup_batch(86_400, 10)
    );
    let removed = a.unwrap().snapshots + b.unwrap().snapshots;
    assert_eq!(removed, 1);
    assert_eq!(
        cleanup.cleanup_batch(86_400, 10).await.unwrap().snapshots,
        0
    );
    teardown(&admin, &schema).await;
}

#[tokio::test]
async fn cleanup_removes_failed_published_and_cancelled_runs() {
    let Some(url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping");
        return;
    };
    let (admin, db, schema) = isolated_database(&url).await;
    let account = provider(db.pool()).await;
    for status in ["failed", "published", "cancelled"] {
        let run = Uuid::now_v7();
        let source = snapshot(db.pool(), account, "superseded").await;
        sqlx::query("INSERT INTO provider_reconciliation_runs (id,provider_account_id,source_snapshot_id,status,partition_count,updated_at) VALUES ($1,$2,$3,$4,1,now()-interval '2 days')")
            .bind(run).bind(account).bind(source).bind(status).execute(db.pool()).await.unwrap();
    }
    let stats = IngestCleanupRepository::new(db.pool().clone())
        .cleanup_batch(86_400, 20)
        .await
        .unwrap();
    assert_eq!(stats.reconciliation_runs, 3);
    teardown(&admin, &schema).await;
}

#[tokio::test]
async fn expired_worker_cannot_process_or_finalize_owned_reconciliation() {
    let Some(url) = database_url() else {
        eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping");
        return;
    };
    let (admin, db, schema) = isolated_database(&url).await;
    let account = provider(db.pool()).await;
    let staged = snapshot(db.pool(), account, "staging").await;
    let parent = Uuid::now_v7();
    let finalizer = Uuid::now_v7();
    let partition_job = Uuid::now_v7();
    for (id, kind) in [
        (parent, "refresh-source"),
        (finalizer, "finalize-provider-reconciliation"),
        (partition_job, "reconcile-provider-partition"),
    ] {
        sqlx::query("INSERT INTO jobs (id,kind,status,locked_by,payload) VALUES ($1,$2,'running','new-worker','{}')").bind(id).bind(kind).execute(db.pool()).await.unwrap();
    }
    let run = Uuid::now_v7();
    sqlx::query("INSERT INTO provider_reconciliation_runs (id,provider_account_id,source_snapshot_id,parent_job_id,status,partition_count) VALUES ($1,$2,$3,$4,'processing',1)")
        .bind(run).bind(account).bind(staged).bind(parent).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO provider_reconciliation_partitions (run_id,partition_number,status,key_count) VALUES ($1,0,'queued',0)").bind(run).execute(db.pool()).await.unwrap();
    let catalog = CatalogRepository::new(db.pool().clone());
    assert!(matches!(
        catalog
            .process_provider_reconciliation_partition_owned(run, 0, "old-worker", partition_job)
            .await,
        Err(PersistenceError::JobOwnership { .. })
    ));
    assert!(matches!(
        catalog
            .finalize_provider_reconciliation_owned(run, parent, finalizer, "old-worker")
            .await,
        Err(PersistenceError::JobNotFound(_))
    ));
    teardown(&admin, &schema).await;
}
