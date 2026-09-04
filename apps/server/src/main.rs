use std::{
    collections::HashMap, env, future::Future, net::SocketAddr, pin::Pin, process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use iptv_api::{AppConfig, AppState, RuntimeVersions, hash_admin_password};
use iptv_ingest::{
    ActivationProgress, ArtifactLimits, DownloadRequest, EndpointProtector, IngestError,
    IngestFormat, IngestProgress, IngestRequest, IngestResult, Ingestor, JobControl,
    ParsedArtifact, PgSnapshotStore, ProtectedEndpoint, SnapshotActivator, SnapshotOwner,
    XtreamEndpoints, XtreamPayloadKind, download_http, parse_artifact_with_source_timezone,
    prepare_snapshot, unpack_artifact,
};
use iptv_media::{
    ProviderSlotBroker, StreamProbe, StreamProbeFailure, StreamProbeOutcome, StreamProbeSpec,
};
#[cfg(test)]
use iptv_persistence::NewJob;
use iptv_persistence::{
    CatalogRepository, DEFAULT_RECONCILIATION_BATCH_SIZE, Database, EpgMappingStats, JobRecord,
    JobRepository, MasterKey, ProviderReconciliationFinalization, ProviderReconciliationPhase,
    ProviderReconciliationProgress, ProviderReconciliationUpdate, ReconcileStats, SourceKind,
    SourceRepository, StreamHealthUpdate, StreamProbeTargetRow,
};
use reqwest::{Client, Url, header::HeaderMap};
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, process::Command, signal, time::sleep};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    match parse_command(env::args().nth(1).as_deref())? {
        GatewayCommand::Serve => serve().await,
        GatewayCommand::Worker => worker().await,
        GatewayCommand::Versions => {
            println!(
                "{}",
                serde_json::to_string_pretty(&runtime_versions().await)?
            );
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GatewayCommand {
    Serve,
    Worker,
    Versions,
}

fn parse_command(argument: Option<&str>) -> Result<GatewayCommand> {
    match argument.unwrap_or("serve") {
        "serve" => Ok(GatewayCommand::Serve),
        "worker" => Ok(GatewayCommand::Worker),
        "versions" => Ok(GatewayCommand::Versions),
        command => bail!("unknown command {command:?}; expected serve, worker, or versions"),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();
}

async fn database() -> Result<Database> {
    let url = required_env("DATABASE_URL")?;
    let database = Database::connect(&url, 10)
        .await
        .context("connect to PostgreSQL")?;
    database
        .migrate()
        .await
        .context("run database migrations")?;
    Ok(database)
}

struct ServerEnvironment {
    bind: SocketAddr,
    public_base_url: String,
    output_token: String,
    admin_bootstrap_token: String,
    admin_password_hash: String,
    master_key: MasterKey,
    tuner_count: u16,
    oidc: Option<iptv_api::OidcConfig>,
}

impl std::fmt::Debug for ServerEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerEnvironment")
            .field("bind", &self.bind)
            .field("public_base_url", &self.public_base_url)
            .field("output_token", &"<redacted>")
            .field("admin_bootstrap_token", &"<redacted>")
            .field("admin_password_hash", &"<redacted>")
            .field("master_key", &self.master_key)
            .field("tuner_count", &self.tuner_count)
            .field("oidc", &self.oidc)
            .finish()
    }
}

impl ServerEnvironment {
    fn into_config(self, runtime_versions: RuntimeVersions) -> AppConfig {
        AppConfig {
            public_base_url: self.public_base_url,
            output_token: self.output_token,
            admin_bootstrap_token: self.admin_bootstrap_token,
            admin_password_hash: self.admin_password_hash,
            master_key: self.master_key,
            tuner_count: self.tuner_count,
            runtime_versions,
            oidc: self.oidc,
        }
    }
}

fn server_environment() -> Result<ServerEnvironment> {
    server_environment_from(|name| env::var(name))
}

fn server_environment_from<F>(mut lookup: F) -> Result<ServerEnvironment>
where
    F: FnMut(&str) -> std::result::Result<String, env::VarError>,
{
    let bind = lookup("IPTV_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8081".to_owned())
        .parse()
        .context("parse IPTV_BIND")?;
    let admin_password_hash = match lookup("IPTV_ADMIN_PASSWORD_HASH") {
        Ok(hash) if !hash.trim().is_empty() => hash,
        _ => {
            warn!("IPTV_ADMIN_PASSWORD_HASH is unset; hashing IPTV_ADMIN_PASSWORD at startup");
            let password = required_password_from(&mut lookup)?;
            hash_admin_password(&password)
                .map_err(anyhow::Error::msg)
                .context("hash administrator password")?
        }
    };

    let public_base_url =
        lookup("IPTV_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_owned());
    let oidc = oidc_config_from(&mut lookup, &public_base_url)?;

    Ok(ServerEnvironment {
        bind,
        public_base_url,
        output_token: required_token_from(&mut lookup, "IPTV_OUTPUT_TOKEN")?,
        admin_bootstrap_token: required_token_from(&mut lookup, "IPTV_ADMIN_BOOTSTRAP_TOKEN")?,
        admin_password_hash,
        master_key: MasterKey::from_base64(&required_env_from(&mut lookup, "IPTV_MASTER_KEY")?)
            .map_err(anyhow::Error::msg)
            .context("parse IPTV_MASTER_KEY")?,
        tuner_count: lookup("IPTV_TUNER_COUNT")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|count| *count > 0)
            .unwrap_or(1),
        oidc,
    })
}

fn oidc_config_from<F>(
    lookup: &mut F,
    public_base_url: &str,
) -> Result<Option<iptv_api::OidcConfig>>
where
    F: FnMut(&str) -> std::result::Result<String, env::VarError>,
{
    let issuer = lookup("IPTV_OIDC_ISSUER_URL").unwrap_or_default();
    let client_id = lookup("IPTV_OIDC_CLIENT_ID").unwrap_or_default();
    let client_secret = lookup("IPTV_OIDC_CLIENT_SECRET").ok();
    let redirect_url = lookup("IPTV_OIDC_REDIRECT_URL").unwrap_or_else(|_| {
        format!(
            "{}/api/v1/auth/oidc/callback",
            public_base_url.trim_end_matches('/')
        )
    });
    let allowed_subjects =
        parse_oidc_entries(&lookup("IPTV_OIDC_ALLOWED_SUBJECTS").unwrap_or_default());
    let allowed_emails =
        parse_oidc_entries(&lookup("IPTV_OIDC_ALLOWED_EMAILS").unwrap_or_default());
    let scopes = parse_oidc_entries(&lookup("IPTV_OIDC_SCOPES").unwrap_or_default());

    let configured = [
        issuer.trim(),
        client_id.trim(),
        client_secret.as_deref().unwrap_or_default().trim(),
        allowed_subjects
            .first()
            .map(String::as_str)
            .unwrap_or_default(),
        allowed_emails
            .first()
            .map(String::as_str)
            .unwrap_or_default(),
    ]
    .iter()
    .any(|value| !value.is_empty());
    if !configured {
        return Ok(None);
    }
    if issuer.trim().is_empty() {
        bail!("IPTV_OIDC_ISSUER_URL is required when OIDC settings are present");
    }
    if client_id.trim().is_empty() {
        bail!("IPTV_OIDC_CLIENT_ID is required when OIDC is enabled");
    }
    iptv_api::OidcConfig::new(
        issuer,
        client_id,
        client_secret,
        redirect_url,
        allowed_subjects,
        allowed_emails,
        scopes,
    )
    .map(Some)
    .map_err(anyhow::Error::msg)
    .context("parse OIDC configuration")
}

fn parse_oidc_entries(value: &str) -> Vec<String> {
    value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

fn required_password_from<F>(lookup: &mut F) -> Result<String>
where
    F: FnMut(&str) -> std::result::Result<String, env::VarError>,
{
    let password = required_env_from(lookup, "IPTV_ADMIN_PASSWORD")?;
    if password.len() < 12 || password.trim() != password {
        bail!("IPTV_ADMIN_PASSWORD must contain at least 12 characters without outer spaces");
    }
    if is_placeholder_secret(&password) {
        bail!("IPTV_ADMIN_PASSWORD cannot use a development or placeholder value");
    }
    Ok(password)
}

fn required_token_from<F>(lookup: &mut F, name: &str) -> Result<String>
where
    F: FnMut(&str) -> std::result::Result<String, env::VarError>,
{
    let token = required_env_from(lookup, name)?;
    let valid_character = |character: char| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '+' | '/' | '=')
    };
    if token.len() < 43 || !token.chars().all(valid_character) {
        bail!("{name} must contain a 256-bit token in hexadecimal or base64 form");
    }
    if is_placeholder_secret(&token) {
        bail!("{name} cannot use a development or placeholder value");
    }
    Ok(token)
}

fn is_placeholder_secret(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    ["change-me", "replace-with", "development", "placeholder"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

async fn serve() -> Result<()> {
    let database = database().await?;
    let environment = server_environment()?;
    let bind = environment.bind;
    let config = environment.into_config(runtime_versions().await);
    let state = AppState::new(Some(database), config);
    state
        .initialize_auth()
        .await
        .context("initialize bootstrap authorization state")?;
    state
        .initialize_output_profile()
        .await
        .context("initialize output profile")?;
    let app = iptv_api::router(state);
    let listener = TcpListener::bind(bind)
        .await
        .context("bind HTTP listener")?;
    info!(%bind, "IPTV Gateway core listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP")?;
    Ok(())
}

fn worker_id_from<F>(mut lookup: F) -> String
where
    F: FnMut(&str) -> std::result::Result<String, env::VarError>,
{
    lookup("IPTV_WORKER_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .or_else(|| lookup("HOSTNAME").ok().filter(|host| !host.is_empty()))
        .unwrap_or_else(|| "worker-1".to_owned())
}

/// Periodically checks for sources due for refresh and enqueues refresh jobs.
///
/// Runs every 60 seconds. Sources with `refresh_interval_seconds > 0` that
/// have not been refreshed within their interval get a new `refresh-source`
/// job enqueued.
async fn refresh_scheduler(jobs: &JobRepository) {
    let interval = Duration::from_mins(1);
    info!("source refresh scheduler started");
    loop {
        tokio::select! {
            () = shutdown_signal() => {
                info!("source refresh scheduler stopping");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
        match jobs.enqueue_due_source_refreshes().await {
            Ok(result) => {
                if !result.is_leader || result.enqueued == 0 {
                    continue;
                }
                info!(
                    enqueued = result.enqueued,
                    "enqueuing scheduled source refreshes"
                );
            }
            Err(error) => {
                warn!(error = %error, "source refresh scheduler failed");
            }
        }
    }
}

async fn worker() -> Result<()> {
    let database = database().await?;
    let master_key = MasterKey::from_base64(&required_env("IPTV_MASTER_KEY")?)
        .map_err(anyhow::Error::msg)
        .context("parse IPTV_MASTER_KEY")?;
    let repository = JobRepository::new(database.pool().clone());
    let sources = SourceRepository::new(database.pool().clone(), master_key.clone());
    let snapshots = PgSnapshotStore::new(database.pool().clone());
    let catalog = CatalogRepository::new(database.pool().clone());
    let worker_id = worker_id_from(|name| env::var(name));
    info!(%worker_id, "job worker started");

    // Recover only expired job leases. A new worker must not requeue a job
    // that another active worker owns.
    match repository.recover_stale_jobs(300).await {
        Ok(count) => info!("Recovered {count} stale jobs on startup"),
        Err(error) => warn!(%error, "stale job recovery failed on startup"),
    }
    match catalog.reset_stranded_checking_streams().await {
        Ok(count) => info!("Reset {count} stranded checking streams on startup"),
        Err(error) => warn!(%error, "stranded checking stream reset failed on startup"),
    }

    // Spawn the scheduler that enqueues refresh jobs for due sources.
    let scheduler_jobs = repository.clone();
    let scheduler_handle = tokio::spawn(async move {
        refresh_scheduler(&scheduler_jobs).await;
    });

    // Spawn the scheduler that enqueues low-priority health probe jobs.
    // Health probes are disabled by default. Set IPTV_HEALTH_PROBES_ENABLED=true
    // to enable them. When enabled, only streams mapped to enabled channels
    // are probed.
    let health_probes_enabled = env::var("IPTV_HEALTH_PROBES_ENABLED")
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"));
    let probe_catalog = catalog.clone();
    let probe_jobs = repository.clone();
    let probe_scheduler_handle = if health_probes_enabled {
        info!("health probe scheduler enabled");
        tokio::spawn(async move {
            health_probe_scheduler(&probe_catalog, &probe_jobs).await;
        })
    } else {
        info!("health probe scheduler disabled (set IPTV_HEALTH_PROBES_ENABLED=true to enable)");
        tokio::spawn(async move {
            // Idle task that does nothing when probes are disabled.
            shutdown_signal().await;
        })
    };

    // Spawn the reaper that resets jobs whose heartbeat is older than the
    // lease timeout. A worker that crashed or lost its lease leaves a job
    // stuck in `running`; the reaper returns it to `queued`.
    let reaper_repository = repository.clone();
    let reaper_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await; // skip the first immediate tick
        loop {
            interval.tick().await;
            match reaper_repository.recover_stale_jobs(300).await {
                Ok(count) if count > 0 => {
                    info!(reaped = count, "stale job reaper reset orphaned jobs");
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(%error, "stale job reaper failed");
                }
            }
        }
    });

    loop {
        tokio::select! {
            () = shutdown_signal() => {
                info!(%worker_id, "worker shutdown requested, finishing current job");
                break;
            }
            result = repository.claim(&worker_id) => {
                match result {
                    Ok(Some(job)) => {
                        info!(job_id = %job.id, kind = %job.kind, "job claimed");
                        if let Err(error) = process_job(
                            &repository,
                            &sources,
                            &snapshots,
                            &catalog,
                            &master_key,
                            &worker_id,
                            job,
                        ).await {
                            error!(%error, "job processing failed");
                            sleep(Duration::from_secs(2)).await;
                        }
                    }
                    Ok(None) => sleep(Duration::from_millis(200)).await,
                    Err(error) => {
                        error!(%error, "job claim failed");
                        sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        }
    }

    scheduler_handle.abort();
    probe_scheduler_handle.abort();
    reaper_handle.abort();
    info!(%worker_id, "worker stopped");
    Ok(())
}

async fn process_job(
    jobs: &JobRepository,
    sources: &SourceRepository,
    snapshots: &PgSnapshotStore,
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    worker_id: &str,
    job: JobRecord,
) -> Result<()> {
    info!(job_id = %job.id, kind = %job.kind, attempts = job.attempts, "job started");
    if job.kind == "noop" {
        jobs.succeed(job.id, worker_id).await?;
        return Ok(());
    }
    if job.kind == "health-probe" {
        return run_health_probe_job(jobs, catalog, master_key, worker_id, &job).await;
    }
    if job.kind == "reconcile-provider-partition" {
        return run_provider_reconciliation_partition_job(jobs, catalog, worker_id, &job).await;
    }
    if job.kind == "finalize-provider-reconciliation" {
        return run_provider_reconciliation_finalizer_job(jobs, catalog, worker_id, &job).await;
    }
    if job.kind != "refresh-source" && job.kind != "refresh-xtream-short-epg" {
        warn!(job_id = %job.id, kind = %job.kind, "unsupported job kind");
        jobs.fail(
            job.id,
            worker_id,
            job.attempts,
            job.max_attempts,
            "job handler is not registered",
        )
        .await?;
        return Ok(());
    }

    run_refresh_job(
        jobs, sources, snapshots, catalog, master_key, worker_id, job,
    )
    .await
}

/// Runs one source or Xtream short-EPG refresh job with a heartbeat task and
/// persists the typed result.
#[allow(clippy::too_many_lines)]
async fn run_refresh_job(
    jobs: &JobRepository,
    sources: &SourceRepository,
    snapshots: &PgSnapshotStore,
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    worker_id: &str,
    job: JobRecord,
) -> Result<()> {
    // Spawn a heartbeat task that keeps the job lease fresh during long
    // downloads. The ingest pipeline reports progress through checkpoints,
    // but a slow download can leave the heartbeat stale. This task touches
    // the heartbeat timestamp every 15 seconds without overwriting the
    // progress data from the last checkpoint.
    let heartbeat_repository = jobs.clone();
    let heartbeat_job_id = job.id;
    let heartbeat_worker_id = worker_id.to_owned();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.tick().await; // skip the first immediate tick
        loop {
            interval.tick().await;
            if heartbeat_repository
                .touch_heartbeat(heartbeat_job_id, &heartbeat_worker_id)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let source_refresh = job.kind == "refresh-source";
    let refresh_result = if source_refresh {
        run_source_refresh(
            jobs, sources, snapshots, catalog, master_key, worker_id, &job,
        )
        .await
    } else {
        run_xtream_short_epg_refresh(
            jobs, sources, snapshots, catalog, master_key, worker_id, &job,
        )
        .await
        .map(|result| RefreshExecution {
            result,
            deferred: false,
        })
    };
    heartbeat_handle.abort();

    match refresh_result {
        Ok(execution) => {
            let result = execution.result;
            if execution.deferred {
                if let Some(source_id) = source_id_from_payload(&job.payload)
                    && let Err(error) = sources
                        .mark_source_refreshed(source_id, chrono::Utc::now())
                        .await
                {
                    warn!(job_id = %job.id, source_id = %source_id, error = %error, "failed to mark source refreshed");
                }
                info!(
                    job_id = %job.id,
                    snapshot_id = %result.snapshot_id,
                    records = result.records,
                    "source refresh staged; waiting for reconciliation finalizer"
                );
                return Ok(());
            }
            if !execution.deferred {
                let completed_progress = completed_refresh_progress(source_refresh, &result);
                if let Err(error) = jobs.heartbeat(job.id, worker_id, &completed_progress).await {
                    warn!(job_id = %job.id, error = %error, "failed to report completed progress");
                }
            }
            jobs.succeed(job.id, worker_id).await?;
            if source_refresh
                && let Some(source_id) = source_id_from_payload(&job.payload)
                && let Err(error) = sources
                    .mark_source_refreshed(source_id, chrono::Utc::now())
                    .await
            {
                warn!(job_id = %job.id, source_id = %source_id, error = %error, "failed to mark source refreshed");
            }
            info!(
                job_id = %job.id,
                snapshot_id = %result.snapshot_id,
                records = result.records,
                downloaded_bytes = result.downloaded_bytes,
                decoded_bytes = result.decoded_bytes,
                "source refresh succeeded"
            );
        }
        Err(RefreshError::Ingest(IngestError::Cancelled)) => {
            info!(job_id = %job.id, "source refresh cancelled");
        }
        Err(error) => {
            if jobs.is_cancelled(job.id).await.unwrap_or(false) {
                info!(job_id = %job.id, "source refresh cancelled");
                return Ok(());
            }
            let summary = error.persisted_summary();
            info!(job_id = %job.id, error = ?error, summary, "source refresh failed");
            if source_refresh && job.attempts >= job.max_attempts {
                jobs.fail_reconciliation_parent_for_terminal_child(
                    job.id,
                    &serde_json::json!({
                        "stage": "failed",
                        "percent": 70,
                        "message": summary,
                    }),
                    summary,
                )
                .await?;
            }
            jobs.fail(job.id, worker_id, job.attempts, job.max_attempts, summary)
                .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_source_refresh(
    jobs: &JobRepository,
    sources: &SourceRepository,
    snapshots: &PgSnapshotStore,
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    worker_id: &str,
    job: &JobRecord,
) -> std::result::Result<RefreshExecution, RefreshError> {
    let source_id = source_id_from_payload(&job.payload).ok_or(RefreshError::InvalidPayload)?;
    let source = sources.load_for_job(source_id).await.map_err(|error| {
        warn!(source_id = %source_id, error = %error, "failed to load source for refresh");
        RefreshError::SourceLoad
    })?;
    let endpoint = Url::parse(&source.endpoint).map_err(|_| {
        info!(source_id = %source_id, endpoint_len = source.endpoint.len(), "source endpoint is not a valid URL");
        RefreshError::InvalidEndpoint
    })?;
    let control = WorkerJobControl {
        repository: jobs.clone(),
        job_id: job.id,
        worker_id: worker_id.to_owned(),
    };
    let protector = StreamEndpointProtector {
        master_key: master_key.clone(),
        associated_data: format!("iptv-provider-stream:v1:{}", source.id),
    };
    let result = if source.kind == SourceKind::Xtream {
        info!(job_id = %job.id, source_id = %source_id, "starting Xtream source refresh");
        run_xtream_refresh(
            endpoint,
            source.id,
            source.timezone,
            control.clone(),
            protector,
            snapshots,
        )
        .await?
    } else {
        let (owner, format) = refresh_target(source.kind, source.id)?;
        info!(
            job_id = %job.id,
            source_id = %source_id,
            format = ?format,
            "starting source download"
        );
        let request = IngestRequest {
            owner,
            format,
            download: DownloadRequest::new(endpoint),
            source_timezone: source.timezone,
            xtream_stream_endpoint: None,
            xtream_category_names: HashMap::new(),
        };
        if source.kind == SourceKind::M3u {
            Ingestor::new(
                protector,
                control.clone(),
                StagedSnapshotActivator::new(snapshots.clone()),
            )
            .run(&request)
            .await
        } else {
            // XMLTV has no deferred provider reconciliation. Publish its
            // snapshot before mapping the guide against active rows.
            Ingestor::new(protector, control.clone(), snapshots.clone())
                .run(&request)
                .await
        }
        .map_err(RefreshError::Ingest)?
    };
    let deferred = if matches!(source.kind, SourceKind::M3u | SourceKind::Xtream) {
        schedule_provider_reconciliation(
            jobs,
            catalog,
            &control,
            &result,
            source.id,
            job.id,
            reconciliation_partition_count(),
        )
        .await?
    } else {
        let reconciliation_progress = WorkerProviderReconciliationProgress {
            control: &control,
            result: &result,
        };
        run_post_refresh_catalog(
            source.kind,
            source.id,
            catalog,
            &control,
            &result,
            &reconciliation_progress,
            reconciliation_batch_size(),
        )
        .await
        .map_err(|error| {
            warn!(job_id = %job.id, source_id = %source.id, error = ?error, "catalog post-refresh work failed");
            error
        })?;
        false
    };
    if source.kind == SourceKind::Xtream {
        match jobs.enqueue_xtream_short_epg(source.id).await {
            Ok(Some(guide_job)) => {
                info!(job_id = %guide_job.id, source_id = %source.id, "scheduled Xtream short EPG refresh");
            }
            Ok(None) => {}
            Err(error) => {
                warn!(source_id = %source.id, error = %error, "failed to schedule Xtream short EPG refresh");
            }
        }
    }
    Ok(RefreshExecution { result, deferred })
}

fn completed_refresh_progress(source_refresh: bool, result: &IngestResult) -> serde_json::Value {
    serde_json::json!({
        "stage": "completed",
        "percent": 100,
        "bytesDownloaded": result.downloaded_bytes,
        "recordsProcessed": result.records,
        "message": if source_refresh {
            "Source refresh completed"
        } else {
            "Xtream short EPG refresh completed"
        },
    })
}

/// Creates one deterministic worker job for each staged provider partition.
///
/// The source refresh only stages immutable rows. Its child workers prepare
/// candidates, and one finalizer later makes the staged catalog public.
async fn schedule_provider_reconciliation(
    jobs: &JobRepository,
    catalog: &CatalogRepository,
    control: &impl JobControl,
    result: &IngestResult,
    source_id: Uuid,
    parent_job_id: Uuid,
    partition_count: i32,
) -> std::result::Result<bool, RefreshError> {
    let Some(run) = catalog
        .create_or_load_provider_reconciliation_run(
            result.snapshot_id,
            parent_job_id,
            partition_count,
        )
        .await
        .map_err(|error| {
            warn!(snapshot_id = %result.snapshot_id, error = %error, "could not create provider reconciliation run");
            RefreshError::Catalog
        })?
    else {
        // A checksum-equivalent snapshot was already active. No background
        // work can make the existing output more complete.
        checkpoint_reconciliation(control, result, "reconciling-complete", 0).await?;
        return Ok(false);
    };

    checkpoint_reconciliation(control, result, "reconciling-partitions", 0).await?;
    for partition_number in 0..run.partition_count {
        jobs.enqueue_provider_reconciliation_partition_if_idle(
            run.id,
            partition_number,
            source_id,
            parent_job_id,
        )
            .await
            .map_err(|error| {
                warn!(run_id = %run.id, partition_number, error = %error, "could not enqueue reconciliation partition");
                RefreshError::Catalog
            })?;
    }
    if !jobs
        .reconciliation_partition_jobs_are_present(run.id)
        .await
        .map_err(|error| {
            warn!(run_id = %run.id, error = %error, "could not verify reconciliation partition jobs");
            RefreshError::Catalog
        })?
    {
        return Err(RefreshError::Catalog);
    }
    // A first finalizer can return `Pending` before worker jobs complete. The
    // final completed partition schedules another attempt.
    jobs.enqueue_provider_reconciliation_finalizer_if_idle(run.id, source_id, parent_job_id)
        .await
        .map_err(|error| {
            warn!(run_id = %run.id, error = %error, "could not enqueue reconciliation finalizer");
            RefreshError::Catalog
        })?;
    info!(
        run_id = %run.id,
        snapshot_id = %result.snapshot_id,
        partition_count = run.partition_count,
        total_keys = run.total_keys,
        "queued staged provider reconciliation"
    );
    Ok(true)
}

/// Prepares one source-snapshot partition and schedules its atomic finalizer.
#[allow(clippy::too_many_lines)]
async fn run_provider_reconciliation_partition_job(
    jobs: &JobRepository,
    catalog: &CatalogRepository,
    worker_id: &str,
    job: &JobRecord,
) -> Result<()> {
    let Some((run_id, partition_number, source_id, parent_job_id)) =
        provider_reconciliation_partition_from_payload(&job.payload)
    else {
        if job.attempts >= job.max_attempts
            && let Some(parent_job_id) = reconciliation_parent_from_payload(&job.payload)
        {
            jobs.fail_reconciliation_parent_for_terminal_child(
                parent_job_id,
                &serde_json::json!({
                    "stage": "failed",
                    "percent": 85,
                    "message": "Reconciliation partition payload remained invalid after retries",
                }),
                "reconciliation partition payload remained invalid after retries",
            )
            .await?;
        }
        jobs.fail(
            job.id,
            worker_id,
            job.attempts,
            job.max_attempts,
            "reconciliation partition payload is invalid",
        )
        .await?;
        return Ok(());
    };
    if !jobs
        .heartbeat_reconciliation_parent(
            parent_job_id,
            &serde_json::json!({
                "stage": "reconciling",
                "percent": 85,
                "runId": run_id,
                "partitionNumber": partition_number,
                "message": "Preparing reconciliation partition",
            }),
        )
        .await?
    {
        return Ok(());
    }
    match catalog
        .process_provider_reconciliation_partition(run_id, partition_number, worker_id)
        .await
    {
        Ok(result) => {
            let aggregate = catalog.provider_reconciliation_progress(run_id).await?;
            let percent = reconciliation_partition_percent(&aggregate);
            jobs.heartbeat_reconciliation_parent(
                parent_job_id,
                &serde_json::json!({
                    "stage": "reconciling",
                    "percent": percent,
                    "runId": run_id,
                    "partitionNumber": partition_number,
                    "keysProcessed": aggregate.completed_keys,
                    "keysTotal": aggregate.total_keys,
                    "partitionsCompleted": aggregate.partitions_completed,
                    "partitionCount": aggregate.partition_count,
                    "message": "Reconciliation partition completed",
                }),
            )
            .await?;
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "reconciling-partition",
                    "percent": percent,
                    "runId": run_id,
                    "partitionNumber": partition_number,
                    "keysProcessed": result.keys_completed,
                    "keysTotal": result.total_keys,
                    "bytesDownloaded": 0,
                    "recordsProcessed": result.keys_completed,
                    "partitionsCompleted": aggregate.partitions_completed,
                    "partitionCount": aggregate.partition_count,
                    "message": if result.already_completed {
                        "Reconciliation partition was already complete"
                    } else {
                        "Reconciliation partition completed"
                    },
                }),
            )
            .await?;
            jobs.complete_provider_reconciliation_partition(
                job.id,
                worker_id,
                run_id,
                source_id,
                parent_job_id,
            )
            .await?;
            Ok(())
        }
        Err(error) => {
            warn!(job_id = %job.id, run_id = %run_id, partition_number, error = %error, "reconciliation partition failed");
            if job.attempts >= job.max_attempts {
                jobs.fail_reconciliation_parent_if_exhausted(
                    parent_job_id,
                    run_id,
                    &serde_json::json!({
                        "stage": "failed",
                        "percent": 85,
                        "runId": run_id,
                        "partitionNumber": partition_number,
                        "message": "Reconciliation partition exhausted retries",
                    }),
                )
                .await?;
            }
            jobs.fail(
                job.id,
                worker_id,
                job.attempts,
                job.max_attempts,
                "reconciliation partition preparation failed",
            )
            .await?;
            Ok(())
        }
    }
}

/// Finalizes complete provider candidates, then runs serial guide work.
#[allow(clippy::too_many_lines)]
async fn run_provider_reconciliation_finalizer_job(
    jobs: &JobRepository,
    catalog: &CatalogRepository,
    worker_id: &str,
    job: &JobRecord,
) -> Result<()> {
    let Some((run_id, _source_id, parent_job_id)) =
        provider_reconciliation_finalizer_from_payload(&job.payload)
    else {
        if job.attempts >= job.max_attempts
            && let Some(parent_job_id) = reconciliation_parent_from_payload(&job.payload)
        {
            jobs.fail_reconciliation_parent_for_terminal_child(
                parent_job_id,
                &serde_json::json!({
                    "stage": "failed",
                    "percent": 99,
                    "message": "Reconciliation finalizer payload remained invalid after retries",
                }),
                "reconciliation finalizer payload remained invalid after retries",
            )
            .await?;
        }
        jobs.fail(
            job.id,
            worker_id,
            job.attempts,
            job.max_attempts,
            "reconciliation finalizer payload is invalid",
        )
        .await?;
        return Ok(());
    };
    if jobs.is_cancelled(parent_job_id).await.unwrap_or(true) {
        return Ok(());
    }
    match catalog
        .finalize_provider_reconciliation(run_id, parent_job_id, job.id)
        .await
    {
        Ok(ProviderReconciliationFinalization::Cancelled) => Ok(()),
        Ok(ProviderReconciliationFinalization::Pending(progress)) => {
            jobs.heartbeat_reconciliation_parent(
                parent_job_id,
                &serde_json::json!({
                    "stage": "reconciling",
                    "percent": reconciliation_partition_percent(&progress),
                    "runId": run_id,
                    "keysProcessed": progress.completed_keys,
                    "keysTotal": progress.total_keys,
                    "partitionsCompleted": progress.partitions_completed,
                    "partitionCount": progress.partition_count,
                    "message": "Waiting for reconciliation partitions",
                }),
            )
            .await?;
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "reconciling-partitions",
                    "percent": reconciliation_partition_percent(&progress),
                    "runId": run_id,
                    "keysProcessed": progress.completed_keys,
                    "keysTotal": progress.total_keys,
                    "partitionsCompleted": progress.partitions_completed,
                    "partitionCount": progress.partition_count,
                    "message": "Waiting for reconciliation partitions",
                }),
            )
            .await?;
            jobs.succeed(job.id, worker_id).await?;
            Ok(())
        }
        Ok(ProviderReconciliationFinalization::Published(_)) => {
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "reconciling-epg",
                    "percent": 99,
                    "runId": run_id,
                    "message": "Reconciling the program guide",
                }),
            )
            .await?;
            // Only the finalizer that atomically published the catalog starts
            // EPG reconciliation. Partition workers never mutate guide rows.
            catalog.reconcile_epg_mappings().await?;
            catalog.scan_all_event_channels().await?;
            jobs.complete_reconciliation_parent(
                parent_job_id,
                run_id,
                &serde_json::json!({
                    "stage": "completed",
                    "percent": 100,
                    "recordsProcessed": 0,
                    "message": "Source refresh completed",
                }),
            )
            .await?;
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "completed",
                    "percent": 100,
                    "runId": run_id,
                    "recordsProcessed": 0,
                    "message": "Source refresh completed",
                }),
            )
            .await?;
            jobs.succeed(job.id, worker_id).await?;
            Ok(())
        }
        Ok(ProviderReconciliationFinalization::AlreadyPublished) => {
            // Publication commits before guide work. Retry guide work when a
            // worker lost its lease after publication but before completion.
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "reconciling-epg",
                    "percent": 99,
                    "runId": run_id,
                    "message": "Retrying the program guide reconciliation",
                }),
            )
            .await?;
            catalog.reconcile_epg_mappings().await?;
            catalog.scan_all_event_channels().await?;
            jobs.complete_reconciliation_parent(
                parent_job_id,
                run_id,
                &serde_json::json!({
                    "stage": "completed",
                    "percent": 100,
                    "recordsProcessed": 0,
                    "message": "Source refresh completed",
                }),
            )
            .await?;
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "completed",
                    "percent": 100,
                    "runId": run_id,
                    "recordsProcessed": 0,
                    "message": "Source refresh completed",
                }),
            )
            .await?;
            jobs.succeed(job.id, worker_id).await?;
            Ok(())
        }
        Ok(ProviderReconciliationFinalization::Superseded) => {
            jobs.complete_reconciliation_parent(
                parent_job_id,
                run_id,
                &serde_json::json!({
                    "stage": "completed",
                    "percent": 100,
                    "recordsProcessed": 0,
                    "message": "Source refresh superseded by a newer refresh",
                }),
            )
            .await?;
            jobs.heartbeat(
                job.id,
                worker_id,
                &serde_json::json!({
                    "stage": "completed",
                    "percent": 100,
                    "runId": run_id,
                    "recordsProcessed": 0,
                    "message": "Source refresh superseded by a newer refresh",
                }),
            )
            .await?;
            jobs.succeed(job.id, worker_id).await?;
            Ok(())
        }
        Err(error) => {
            warn!(job_id = %job.id, run_id = %run_id, error = %error, "reconciliation finalizer failed");
            if job.attempts >= job.max_attempts {
                jobs.fail_reconciliation_parent_for_terminal_child(
                    parent_job_id,
                    &serde_json::json!({
                        "stage": "failed",
                        "percent": 99,
                        "runId": run_id,
                        "message": "Reconciliation finalizer exhausted retries",
                    }),
                    "reconciliation finalizer exhausted retries",
                )
                .await?;
            }
            jobs.fail(
                job.id,
                worker_id,
                job.attempts,
                job.max_attempts,
                "reconciliation finalization failed",
            )
            .await?;
            Ok(())
        }
    }
}

fn provider_reconciliation_run_from_payload(payload: &serde_json::Value) -> Option<Uuid> {
    payload.get("runId")?.as_str()?.parse().ok()
}

fn reconciliation_parent_from_payload(payload: &serde_json::Value) -> Option<Uuid> {
    payload.get("parentJobId")?.as_str()?.parse().ok()
}

fn provider_reconciliation_partition_from_payload(
    payload: &serde_json::Value,
) -> Option<(Uuid, i32, Uuid, Uuid)> {
    let run_id = provider_reconciliation_run_from_payload(payload)?;
    let partition_number = payload.get("partitionNumber")?.as_str()?.parse().ok()?;
    let source_id = payload.get("sourceId")?.as_str()?.parse().ok()?;
    let parent_job_id = payload.get("parentJobId")?.as_str()?.parse().ok()?;
    if partition_number < 0 {
        return None;
    }
    Some((run_id, partition_number, source_id, parent_job_id))
}

fn provider_reconciliation_finalizer_from_payload(
    payload: &serde_json::Value,
) -> Option<(Uuid, Uuid, Uuid)> {
    Some((
        provider_reconciliation_run_from_payload(payload)?,
        payload.get("sourceId")?.as_str()?.parse().ok()?,
        payload.get("parentJobId")?.as_str()?.parse().ok()?,
    ))
}

fn reconciliation_partition_percent(
    progress: &iptv_persistence::ProviderReconciliationRunProgress,
) -> i64 {
    if progress.total_keys <= 0 {
        return 85;
    }
    let completed = progress.completed_keys.clamp(0, progress.total_keys);
    85 + completed.saturating_mul(14) / progress.total_keys
}

const MAX_SHORT_EPG_STREAMS: i64 = 16;
const SHORT_EPG_PROGRAMME_LIMIT: u16 = 4;

#[allow(clippy::too_many_lines)]
async fn run_xtream_short_epg_refresh(
    jobs: &JobRepository,
    sources: &SourceRepository,
    snapshots: &PgSnapshotStore,
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    worker_id: &str,
    job: &JobRecord,
) -> std::result::Result<IngestResult, RefreshError> {
    let source_id = source_id_from_payload(&job.payload).ok_or(RefreshError::InvalidPayload)?;
    let source = sources.load_for_job(source_id).await.map_err(|error| {
        warn!(source_id = %source_id, error = %error, "failed to load Xtream source for short EPG refresh");
        RefreshError::SourceLoad
    })?;
    if source.kind != SourceKind::Xtream {
        return Err(RefreshError::UnsupportedSource);
    }
    let endpoint = Url::parse(&source.endpoint).map_err(|_| RefreshError::InvalidEndpoint)?;
    let endpoints =
        XtreamEndpoints::from_player_api_url(endpoint.as_str()).map_err(|error| {
            info!(source_id = %source.id, error = %error, "Xtream short EPG endpoint validation failed");
            RefreshError::Ingest(error)
        })?;
    let control = WorkerJobControl {
        repository: jobs.clone(),
        job_id: job.id,
        worker_id: worker_id.to_owned(),
    };
    control
        .checkpoint(&IngestProgress {
            phase: "downloading".to_owned(),
            ..IngestProgress::default()
        })
        .await
        .map_err(RefreshError::Ingest)?;

    info!(
        source_id = %source.id,
        request = ?endpoints.authentication_request(),
        "Xtream short EPG auth request started"
    );
    let authentication = fetch_xtream_payload(
        endpoints.authentication_request(),
        XtreamPayloadKind::Auth,
        &source.timezone,
    )
    .await
    .map_err(RefreshError::Ingest)?;
    match authentication.parsed {
        ParsedArtifact::XtreamAuth(document)
            if document.records.iter().any(|record| record.authenticated) =>
        {
            info!(source_id = %source.id, "Xtream short EPG auth succeeded");
        }
        ParsedArtifact::XtreamAuth(_) => {
            info!(source_id = %source.id, "Xtream short EPG auth rejected the credentials");
            return Err(RefreshError::Ingest(IngestError::InvalidRequest(
                "Xtream authentication was rejected",
            )));
        }
        _ => {
            info!(source_id = %source.id, "Xtream short EPG auth response had an unexpected payload");
            return Err(RefreshError::Ingest(IngestError::InvalidRequest(
                "Xtream authentication response has an unexpected payload",
            )));
        }
    }

    let streams = sources
        .list_xtream_short_epg_streams(source.id, MAX_SHORT_EPG_STREAMS)
        .await
        .map_err(|error| {
            warn!(source_id = %source.id, error = %error, "failed to select Xtream short EPG streams");
            RefreshError::SourceLoad
        })?;
    if streams.is_empty() {
        return Err(RefreshError::Ingest(IngestError::EmptySnapshot {
            format: "Xtream short EPG",
        }));
    }

    let protector = StreamEndpointProtector {
        master_key: master_key.clone(),
        associated_data: format!("iptv-provider-stream:v1:{}", source.id),
    };
    let mut records = Vec::new();
    let mut diagnostics = Vec::new();
    let mut stats = iptv_parsers::ParseStats::default();
    let mut downloaded_bytes = 0_u64;
    let mut decoded_bytes = 0_u64;
    let mut checksum = Sha256::new();
    for stream in streams {
        control
            .checkpoint(&IngestProgress {
                phase: "downloading".to_owned(),
                downloaded_bytes,
                decoded_bytes,
                records_seen: stats.records_seen,
                records_prepared: u64::try_from(records.len()).unwrap_or(u64::MAX),
            })
            .await
            .map_err(RefreshError::Ingest)?;
        let payload = fetch_xtream_payload(
            endpoints.short_epg_request(
                u64::try_from(stream.stream_id).map_err(|_| {
                    RefreshError::Ingest(IngestError::InvalidRequest(
                        "Xtream stream identifier is invalid",
                    ))
                })?,
                SHORT_EPG_PROGRAMME_LIMIT,
            ),
            XtreamPayloadKind::ShortEpg,
            &source.timezone,
        )
        .await
        .map_err(RefreshError::Ingest)?;
        downloaded_bytes = downloaded_bytes.saturating_add(payload.downloaded_bytes);
        decoded_bytes = decoded_bytes.saturating_add(payload.decoded_bytes);
        checksum.update(payload.checksum_sha256.as_bytes());
        let ParsedArtifact::XtreamEpg(mut document) = payload.parsed else {
            return Err(RefreshError::Ingest(IngestError::InvalidRequest(
                "Xtream short EPG response has an unexpected payload",
            )));
        };
        stats.bytes_read = stats.bytes_read.saturating_add(document.stats.bytes_read);
        stats.records_seen = stats
            .records_seen
            .saturating_add(document.stats.records_seen);
        stats.records_emitted = stats
            .records_emitted
            .saturating_add(document.stats.records_emitted);
        stats.records_skipped = stats
            .records_skipped
            .saturating_add(document.stats.records_skipped);
        stats.warnings = stats.warnings.saturating_add(document.stats.warnings);
        stats.errors = stats.errors.saturating_add(document.stats.errors);
        diagnostics.append(&mut document.diagnostics);
        for entry in &mut document.records {
            if entry.channel_id.is_none() && entry.epg_id.is_none() {
                entry.channel_id = Some(stream.channel_id.clone());
            }
            entry.metadata.insert(
                "requested_stream_id".to_owned(),
                serde_json::json!(stream.stream_id),
            );
        }
        records.extend(document.records);
    }

    let request = IngestRequest {
        owner: SnapshotOwner::ProviderAccount(source.id),
        format: IngestFormat::Xtream(XtreamPayloadKind::ShortEpg),
        download: endpoints.authentication_request(),
        source_timezone: source.timezone,
        xtream_stream_endpoint: None,
        xtream_category_names: HashMap::new(),
    };
    let parsed = ParsedArtifact::XtreamEpg(iptv_parsers::XtreamDocument {
        records,
        diagnostics,
        stats,
    });
    let snapshot = prepare_snapshot(
        &request,
        parsed,
        format!("{:x}", checksum.finalize()),
        downloaded_bytes,
        &protector,
    )
    .map_err(RefreshError::Ingest)?;
    control
        .checkpoint(&IngestProgress {
            phase: "staging".to_owned(),
            downloaded_bytes,
            decoded_bytes,
            records_seen: snapshot.record_count,
            records_prepared: 0,
        })
        .await
        .map_err(RefreshError::Ingest)?;
    control
        .begin_activation()
        .await
        .map_err(RefreshError::Ingest)?;
    let staging_progress = WorkerStagingProgress {
        control: &control,
        downloaded_bytes,
        decoded_bytes,
        records_seen: snapshot.record_count,
    };
    let snapshot_id = snapshots
        .activate_with_progress(&snapshot, &staging_progress)
        .await
        .map_err(RefreshError::Ingest)?;
    let result = IngestResult {
        snapshot_id,
        checksum_sha256: snapshot.checksum_sha256,
        downloaded_bytes,
        decoded_bytes,
        records: snapshot.record_count,
    };
    checkpoint_reconciliation(&control, &result, "reconciling-epg", 0).await?;
    let mapping_stats = catalog.reconcile_epg_mappings().await.map_err(|error| {
        warn!(source_id = %source.id, error = %error, "Xtream short EPG reconciliation failed");
        RefreshError::Catalog
    })?;
    checkpoint_reconciliation(
        &control,
        &result,
        "reconciling-events",
        u64::try_from(mapping_stats.mappings_applied.max(0)).unwrap_or(u64::MAX),
    )
    .await?;
    let event_count = catalog.scan_all_event_channels().await.map_err(|error| {
        warn!(source_id = %source.id, error = %error, "Xtream short EPG event scan failed");
        RefreshError::Catalog
    })?;
    checkpoint_reconciliation(&control, &result, "reconciling-complete", event_count).await?;
    Ok(result)
}

/// The low-priority pool capacity for health probes.
///
/// The worker-local broker limits probes within one worker process. Cross-process
/// admission must also coordinate with the core media broker before production use.
const HEALTH_PROBE_POOL_CAPACITY: usize = 1;
/// The minimum transport packets required before a probe declares a stream
/// alive.
const HEALTH_PROBE_MIN_PACKETS: usize = 32;
/// The maximum wall-clock duration for one probe window.
const HEALTH_PROBE_MAX_DURATION: Duration = Duration::from_secs(8);
/// The startup timeout for the upstream to return response headers.
const HEALTH_PROBE_STARTUP_TIMEOUT: Duration = Duration::from_secs(4);
/// The interval at which the health probe scheduler runs.
const HEALTH_PROBE_SCHEDULER_INTERVAL: Duration = Duration::from_mins(2);
/// A stream in `checking` status longer than this is considered stranded.
const HEALTH_PROBE_STRANDED_TIMEOUT_SECONDS: i64 = 300;
/// The job priority for health-probe jobs. Jobs are claimed in descending
/// priority order, so a value below the refresh job priority of `0` keeps
/// health probes behind source refreshes when the queue is contested.
const HEALTH_PROBE_JOB_PRIORITY: i32 = -1;
/// The maximum retry attempts for one health-probe job.
const HEALTH_PROBE_JOB_MAX_ATTEMPTS: i32 = 2;

/// Handles one `health-probe` job. Loads the stream target, decrypts the URL,
/// acquires a low-priority provider slot, runs the probe, and persists the
/// result with re-ranking.
async fn run_health_probe_job(
    jobs: &JobRepository,
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    worker_id: &str,
    job: &JobRecord,
) -> Result<()> {
    let Some(provider_stream_id) = provider_stream_id_from_payload(&job.payload) else {
        jobs.fail(
            job.id,
            worker_id,
            job.attempts,
            job.max_attempts,
            "health probe payload is invalid",
        )
        .await?;
        return Ok(());
    };
    let outcome = run_health_probe(catalog, master_key, provider_stream_id).await;
    match outcome {
        Ok(HealthProbeResult::Skipped) => {
            // The probe did not run: the stream was already being probed by
            // another worker, or the low-priority pool was full. Succeed the
            // job without a health update so the known-good status and ranking
            // are preserved. The scheduler re-enqueues the stream later.
            jobs.succeed(job.id, worker_id).await?;
        }
        Ok(HealthProbeResult::Applied | HealthProbeResult::Missing) => {
            jobs.succeed(job.id, worker_id).await?;
        }
        Err(error) => {
            let summary = format!("{error:#}");
            warn!(job_id = %job.id, error = %summary, "health probe failed");
            jobs.fail(job.id, worker_id, job.attempts, job.max_attempts, &summary)
                .await?;
        }
    }
    Ok(())
}

/// The outcome of one health probe attempt.
#[derive(Debug, Eq, PartialEq)]
enum HealthProbeResult {
    /// The probe ran and persisted a health update.
    Applied,
    /// The probe did not run because the stream was already being probed or the
    /// low-priority pool was full. No health update is persisted.
    Skipped,
    /// The stream target was missing or disabled.
    Missing,
}

/// Runs one health probe and persists the typed, redacted result.
///
/// The probe first claims the stream with an atomic `unknown`/`dead` to
/// `checking` transition. A second worker that targets the same stream sees
/// the `checking` status and returns [`HealthProbeResult::Skipped`] without
/// running a probe or persisting an update. A skipped probe never overwrites a
/// known-good health status or quality ranking.
async fn run_health_probe(
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    provider_stream_id: Uuid,
) -> std::result::Result<HealthProbeResult, anyhow::Error> {
    run_health_probe_with_core_config(catalog, master_key, provider_stream_id, core_probe_config())
        .await
}

fn core_probe_config() -> Option<(String, String)> {
    core_probe_config_from(
        env::var("IPTV_CORE_INTERNAL_URL").ok(),
        env::var("IPTV_ADMIN_BOOTSTRAP_TOKEN").ok(),
    )
}

fn core_probe_config_from(
    core_url: Option<String>,
    token: Option<String>,
) -> Option<(String, String)> {
    match (core_url, token) {
        (Some(core_url), Some(token))
            if !core_url.trim().is_empty() && !token.trim().is_empty() =>
        {
            Some((core_url, token))
        }
        _ => None,
    }
}

async fn run_health_probe_with_core_config(
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    provider_stream_id: Uuid,
    core_config: Option<(String, String)>,
) -> std::result::Result<HealthProbeResult, anyhow::Error> {
    let Some(target) = catalog.load_stream_probe_target(provider_stream_id).await? else {
        return Ok(HealthProbeResult::Missing);
    };
    // Atomically claim the stream so two workers cannot probe it at the same
    // time. A stream that is already `checking` or `alive` is left untouched.
    let claimed = catalog.try_mark_stream_checking(provider_stream_id).await?;
    if !claimed {
        info!(
            provider_stream_id = %provider_stream_id,
            "health probe skipped: stream is already being probed or is alive"
        );
        return Ok(HealthProbeResult::Skipped);
    }
    let url = decrypt_probe_url(master_key, &target)?;
    let distributed_reservation = match core_config {
        Some((core_url, token)) => {
            if let Some(reservation) =
                reserve_core_probe_slot(&core_url, &token, &target.provider_pool_id).await?
            {
                Some(reservation)
            } else {
                catalog
                    .release_stream_checking_claim(provider_stream_id)
                    .await?;
                return Ok(HealthProbeResult::Skipped);
            }
        }
        None => None,
    };
    let pool_id = format!("{}:probe", target.provider_pool_id);
    let broker = ProviderSlotBroker::new(pool_id, HEALTH_PROBE_POOL_CAPACITY);
    let client = Client::new();
    let spec = StreamProbeSpec {
        url: url.into(),
        headers: HeaderMap::new(),
        pool_id: target.provider_pool_id.to_string().into(),
        session_key: provider_stream_id.to_string().into(),
        max_connections: HEALTH_PROBE_POOL_CAPACITY,
        startup_timeout: HEALTH_PROBE_STARTUP_TIMEOUT,
        max_duration: HEALTH_PROBE_MAX_DURATION,
        min_packets: HEALTH_PROBE_MIN_PACKETS,
    };
    let probe = StreamProbe::new(broker, client);
    let outcome = probe.run(&spec).await;
    if let Some(reservation) = distributed_reservation {
        reservation.release().await;
    }
    // A skipped probe (low-priority pool full) must not persist an update. The
    // `stream_health_checks.status` check constraint only allows `alive`,
    // `dead`, and `error`, so an `unknown` row would violate it. Skipping the
    // update also preserves the known-good health status and ranking.
    if let Some(update) = stream_health_update_for(provider_stream_id, &outcome) {
        catalog.update_stream_health_and_rank(&update).await?;
    }
    info!(
        provider_stream_id = %provider_stream_id,
        outcome = ?outcome,
        "health probe completed"
    );
    Ok(if matches!(outcome, StreamProbeOutcome::Skipped) {
        HealthProbeResult::Skipped
    } else {
        HealthProbeResult::Applied
    })
}

struct CoreProbeReservation {
    client: Client,
    url: String,
    token: String,
}

impl CoreProbeReservation {
    async fn release(self) {
        let _ = self
            .client
            .delete(&self.url)
            .bearer_auth(&self.token)
            .send()
            .await;
    }
}

async fn reserve_core_probe_slot(
    core_url: &str,
    token: &str,
    pool_id: &Uuid,
) -> Result<Option<CoreProbeReservation>, anyhow::Error> {
    let client = Client::new();
    let base = core_url.trim_end_matches('/');
    let collection = format!("{base}/internal/v1/provider-reservations");
    let response = client
        .post(&collection)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "pool_id": pool_id,
            "capacity": HEALTH_PROBE_POOL_CAPACITY,
        }))
        .send()
        .await
        .context("reserve a core provider slot for health probe")?;
    if response.status() == reqwest::StatusCode::CONFLICT {
        return Ok(None);
    }
    let response = response.error_for_status()?;
    let body: serde_json::Value = response.json().await?;
    let reservation_id = body
        .get("reservation_id")
        .and_then(serde_json::Value::as_str)
        .context("core provider reservation omitted its id")?
        .to_owned();
    Ok(Some(CoreProbeReservation {
        client,
        url: format!("{collection}/{reservation_id}"),
        token: token.to_owned(),
    }))
}

/// Decrypts the probe target URL. Falls back to the template when no
/// ciphertext is present.
fn decrypt_probe_url(
    master_key: &MasterKey,
    target: &StreamProbeTargetRow,
) -> std::result::Result<String, anyhow::Error> {
    let Some(ciphertext) = target.url_secret_ciphertext.as_deref() else {
        return Ok(target.url_template.clone());
    };
    let associated_data = format!("iptv-provider-stream:v1:{}", target.provider_account_id);
    let plaintext = master_key
        .decrypt_secret(ciphertext, associated_data.as_bytes())
        .map_err(anyhow::Error::msg)
        .context("decrypt stream URL for health probe")?;
    String::from_utf8(plaintext).context("decrypted stream URL is not valid UTF-8")
}

/// Builds a typed, redacted [`StreamHealthUpdate`] from a probe outcome.
///
/// Returns `None` for a skipped probe. A skipped probe must not persist an
/// update because the `stream_health_checks.status` check constraint only
/// allows `alive`, `dead`, and `error`, and because a skip must not overwrite a
/// known-good health status or quality ranking.
fn stream_health_update_for(
    provider_stream_id: Uuid,
    outcome: &StreamProbeOutcome,
) -> Option<StreamHealthUpdate> {
    match outcome {
        StreamProbeOutcome::Alive {
            quality,
            packets_seen: _,
            duration_ms,
        } => Some(StreamHealthUpdate {
            provider_stream_id,
            status: "alive".to_owned(),
            error: None,
            video_codec: quality.video_codec.clone(),
            video_resolution: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            audio_codec: quality.audio_codec.clone(),
            audio_channels: None,
            audio_sample_rate: None,
            bitrate_kbps: None,
            check_duration_ms: Some(i32::try_from(*duration_ms).unwrap_or(i32::MAX)),
        }),
        StreamProbeOutcome::Dead(failure) => Some(StreamHealthUpdate {
            provider_stream_id,
            status: "dead".to_owned(),
            error: Some(redacted_failure_message(failure)),
            video_codec: None,
            video_resolution: None,
            video_width: None,
            video_height: None,
            video_fps: None,
            audio_codec: None,
            audio_channels: None,
            audio_sample_rate: None,
            bitrate_kbps: None,
            check_duration_ms: None,
        }),
        StreamProbeOutcome::Skipped => None,
    }
}

/// Returns a redacted, human-readable failure message. No URL or credential
/// data is included.
fn redacted_failure_message(failure: &StreamProbeFailure) -> String {
    match failure {
        StreamProbeFailure::CapacityFull => "provider pool at capacity".to_owned(),
        StreamProbeFailure::StartupTimeout => "upstream startup timeout".to_owned(),
        StreamProbeFailure::Http => "upstream HTTP request failed".to_owned(),
        StreamProbeFailure::HttpStatus => "upstream returned an unsuccessful status".to_owned(),
        StreamProbeFailure::NoPat => "no PAT section observed".to_owned(),
        StreamProbeFailure::NoPmt => "no PMT section observed".to_owned(),
        StreamProbeFailure::NoVideo => "no video elementary stream declared".to_owned(),
        StreamProbeFailure::NoAudio => "no audio elementary stream declared".to_owned(),
        StreamProbeFailure::InsufficientPackets { seen, required } => {
            format!("insufficient packet flow: saw {seen} of {required} required packets")
        }
        StreamProbeFailure::MalformedTransport => "transport stream data was malformed".to_owned(),
    }
}

/// Extracts the provider stream ID from a `health-probe` job payload.
fn provider_stream_id_from_payload(payload: &serde_json::Value) -> Option<Uuid> {
    payload
        .get("providerStreamId")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// Periodically enqueues low-priority health probe jobs for streams that need
/// a check. Recovers stranded `checking` records before each cycle.
async fn health_probe_scheduler(catalog: &CatalogRepository, jobs: &JobRepository) {
    info!("health probe scheduler started");
    let core_url = env::var("IPTV_CORE_INTERNAL_URL").ok();
    let core_client = Client::new();
    loop {
        tokio::select! {
            () = shutdown_signal() => {
                info!("health probe scheduler stopping");
                return;
            }
            () = tokio::time::sleep(HEALTH_PROBE_SCHEDULER_INTERVAL) => {}
        }
        if should_defer_health_probes(&core_client, core_url.as_deref()).await {
            debug!("deferring health probes while playback sessions are active");
            continue;
        }
        if let Err(error) = catalog
            .recover_stranded_checking_streams(HEALTH_PROBE_STRANDED_TIMEOUT_SECONDS)
            .await
        {
            warn!(error = %error, "health probe scheduler failed to recover stranded checking streams");
        }
        match catalog
            .list_streams_for_health_probe(50, HEALTH_PROBE_STRANDED_TIMEOUT_SECONDS)
            .await
        {
            Ok(streams) => {
                if streams.is_empty() {
                    continue;
                }
                debug!(
                    probe_count = streams.len(),
                    "enqueuing scheduled health probes"
                );
                for stream in streams {
                    // Enqueue only when no queued or running health-probe job
                    // already targets this stream. The atomic guard prevents
                    // duplicate per-worker probes across overlapping cycles.
                    match jobs
                        .enqueue_health_probe_if_idle(
                            stream.provider_stream_id,
                            HEALTH_PROBE_JOB_PRIORITY,
                            HEALTH_PROBE_JOB_MAX_ATTEMPTS,
                        )
                        .await
                    {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            debug!(
                                provider_stream_id = %stream.provider_stream_id,
                                "health probe job already queued or running"
                            );
                        }
                        Err(error) => {
                            warn!(
                                provider_stream_id = %stream.provider_stream_id,
                                error = %error,
                                "failed to enqueue scheduled health probe"
                            );
                        }
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, "health probe scheduler failed to list streams for probe");
            }
        }
    }
}

/// Return true when the core reports at least one active provider session.
///
/// Workers use this check to defer low-priority probes during playback. The
/// check is conservative: an unavailable core also defers probes.
async fn core_has_active_sessions(client: &Client, core_url: &str) -> bool {
    let url = format!("{}/metrics", core_url.trim_end_matches('/'));
    let Ok(response) = client.get(url).send().await else {
        return true;
    };
    let Ok(body) = response.bytes().await else {
        return true;
    };
    let Ok(body) = std::str::from_utf8(&body) else {
        return true;
    };
    metrics_report_active_sessions(body)
}

async fn should_defer_health_probes(client: &Client, core_url: Option<&str>) -> bool {
    match core_url {
        Some(core_url) => core_has_active_sessions(client, core_url).await,
        None => false,
    }
}

fn metrics_report_active_sessions(body: &str) -> bool {
    body.lines().any(|line| {
        line.strip_prefix("iptv_media_provider_active_sessions ")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .is_some_and(|active| active > 0)
    })
}

#[allow(clippy::too_many_lines)]
async fn run_xtream_refresh(
    endpoint: Url,
    source_id: Uuid,
    source_timezone: String,
    control: WorkerJobControl,
    protector: StreamEndpointProtector,
    snapshots: &PgSnapshotStore,
) -> std::result::Result<IngestResult, RefreshError> {
    let endpoints = XtreamEndpoints::from_player_api_url(endpoint.as_str()).map_err(|error| {
        info!(source_id = %source_id, error = %error, "Xtream endpoint validation failed");
        RefreshError::Ingest(error)
    })?;
    info!(
        source_id = %source_id,
        endpoints = ?endpoints,
        "Xtream endpoints derived"
    );

    control
        .checkpoint(&IngestProgress {
            phase: "downloading".to_owned(),
            ..IngestProgress::default()
        })
        .await
        .map_err(RefreshError::Ingest)?;
    info!(
        source_id = %source_id,
        request = ?endpoints.authentication_request(),
        "Xtream auth request started"
    );
    let authentication = fetch_xtream_payload(
        endpoints.authentication_request(),
        XtreamPayloadKind::Auth,
        &source_timezone,
    )
    .await
    .map_err(RefreshError::Ingest)?;
    match authentication.parsed {
        ParsedArtifact::XtreamAuth(document)
            if document.records.iter().any(|record| record.authenticated) =>
        {
            info!(source_id = %source_id, "Xtream auth succeeded");
        }
        ParsedArtifact::XtreamAuth(_) => {
            info!(source_id = %source_id, "Xtream auth rejected the credentials");
            return Err(RefreshError::Ingest(IngestError::InvalidRequest(
                "Xtream authentication was rejected",
            )));
        }
        _ => {
            info!(source_id = %source_id, "Xtream auth response had an unexpected payload");
            return Err(RefreshError::Ingest(IngestError::InvalidRequest(
                "Xtream authentication response has an unexpected payload",
            )));
        }
    }

    info!(
        source_id = %source_id,
        request = ?endpoints.live_categories_request(),
        "Xtream live categories request started"
    );
    let category_names = match fetch_xtream_payload(
        endpoints.live_categories_request(),
        XtreamPayloadKind::LiveCategories,
        &source_timezone,
    )
    .await
    {
        Ok(XtreamPayload {
            parsed: ParsedArtifact::XtreamCategories(document),
            ..
        }) => {
            info!(source_id = %source_id, categories = document.records.len(), "Xtream live categories succeeded");
            document
                .records
                .into_iter()
                .map(|category| (category.id, category.name))
                .collect()
        }
        Ok(_) => {
            info!(source_id = %source_id, "Xtream category response had an unexpected payload");
            return Err(RefreshError::Ingest(IngestError::InvalidRequest(
                "Xtream category response has an unexpected payload",
            )));
        }
        Err(IngestError::EmptySnapshot { .. }) => {
            info!(source_id = %source_id, "Xtream live categories returned no records");
            HashMap::new()
        }
        Err(error) => return Err(RefreshError::Ingest(error)),
    };

    let live_streams_request = endpoints.live_streams_request();
    info!(
        source_id = %source_id,
        request = ?live_streams_request,
        "Xtream live streams request started"
    );
    let request = IngestRequest {
        owner: SnapshotOwner::ProviderAccount(source_id),
        format: IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
        download: live_streams_request,
        source_timezone,
        xtream_stream_endpoint: Some(endpoints.into_stream_template()),
        xtream_category_names: category_names,
    };
    Ingestor::new(
        protector,
        control,
        StagedSnapshotActivator::new(snapshots.clone()),
    )
    .run(&request)
    .await
    .map_err(RefreshError::Ingest)
}

async fn fetch_xtream_payload(
    request: DownloadRequest,
    kind: XtreamPayloadKind,
    source_timezone: &str,
) -> std::result::Result<XtreamPayload, IngestError> {
    let downloaded = download_http(&request, ArtifactLimits::default()).await.map_err(|error| {
        info!(kind = ?kind, request = ?request, error = %error, "Xtream payload download failed");
        error
    })?;
    let downloaded_bytes = downloaded.byte_count;
    let decoded =
        tokio::task::spawn_blocking(move || unpack_artifact(downloaded, ArtifactLimits::default()))
            .await
            .map_err(|_| IngestError::ArtifactIo(std::io::Error::other("decode task failed")))??;
    let decoded_bytes = decoded.decoded_byte_count;
    let checksum_sha256 = decoded.sha256.clone();
    let source_timezone = source_timezone.to_owned();
    tokio::task::spawn_blocking(move || {
        parse_artifact_with_source_timezone(
            &decoded,
            IngestFormat::Xtream(kind),
            iptv_parsers::ParseLimits::default(),
            &source_timezone,
        )
    })
    .await
    .map_err(|_| IngestError::ArtifactIo(std::io::Error::other("parse task failed")))?
    .map(|parsed| {
        info!(
            kind = ?kind,
            request = ?request,
            downloaded_bytes,
            decoded_bytes,
            records = parsed.usable_record_count(),
            "Xtream payload fetched"
        );
        XtreamPayload {
            parsed,
            downloaded_bytes,
            decoded_bytes,
            checksum_sha256,
        }
    })
}

#[derive(Debug)]
struct XtreamPayload {
    parsed: ParsedArtifact,
    downloaded_bytes: u64,
    decoded_bytes: u64,
    checksum_sha256: String,
}

type ProviderReconcileFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<ReconcileStats, iptv_persistence::PersistenceError>> + Send + 'a,
    >,
>;
type EpgReconcileFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<EpgMappingStats, iptv_persistence::PersistenceError>>
            + Send
            + 'a,
    >,
>;
type EventReconcileFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u64, iptv_persistence::PersistenceError>> + Send + 'a>>;

trait PostRefreshCatalog {
    fn reconcile_provider_account_with_progress<'a>(
        &'a self,
        source_id: Uuid,
        progress: &'a dyn ProviderReconciliationProgress,
        batch_size: i64,
    ) -> ProviderReconcileFuture<'a>;
    fn reconcile_epg_mappings(&self) -> EpgReconcileFuture<'_>;
    fn scan_all_event_channels(&self) -> EventReconcileFuture<'_>;
}

impl PostRefreshCatalog for CatalogRepository {
    fn reconcile_provider_account_with_progress<'a>(
        &'a self,
        source_id: Uuid,
        progress: &'a dyn ProviderReconciliationProgress,
        batch_size: i64,
    ) -> ProviderReconcileFuture<'a> {
        Box::pin(async move {
            CatalogRepository::reconcile_provider_account_with_progress_and_batch(
                self, source_id, progress, batch_size,
            )
            .await
        })
    }

    fn reconcile_epg_mappings(&self) -> EpgReconcileFuture<'_> {
        Box::pin(async move { CatalogRepository::reconcile_epg_mappings(self).await })
    }

    fn scan_all_event_channels(&self) -> EventReconcileFuture<'_> {
        Box::pin(async move { CatalogRepository::scan_all_event_channels(self).await })
    }
}

async fn run_post_refresh_catalog<C: PostRefreshCatalog>(
    source_kind: SourceKind,
    source_id: Uuid,
    catalog: &C,
    control: &impl JobControl,
    result: &IngestResult,
    provider_progress: &dyn ProviderReconciliationProgress,
    batch_size: i64,
) -> std::result::Result<(), RefreshError> {
    match source_kind {
        SourceKind::M3u | SourceKind::Xtream => {
            checkpoint_reconciliation(control, result, "reconciling-channels", 0).await?;
            let channel_stats = catalog
                .reconcile_provider_account_with_progress(source_id, provider_progress, batch_size)
                .await
                .map_err(|_| RefreshError::Catalog)?;
            checkpoint_reconciliation(
                control,
                result,
                "reconciling-epg",
                u64::try_from(channel_stats.channels.max(0)).unwrap_or(u64::MAX),
            )
            .await?;
            let mapping_stats = catalog
                .reconcile_epg_mappings()
                .await
                .map_err(|_| RefreshError::Catalog)?;
            checkpoint_reconciliation(
                control,
                result,
                "reconciling-events",
                u64::try_from(mapping_stats.mappings_applied.max(0)).unwrap_or(u64::MAX),
            )
            .await?;
            let event_count = catalog
                .scan_all_event_channels()
                .await
                .map_err(|_| RefreshError::Catalog)?;
            checkpoint_reconciliation(control, result, "reconciling-complete", event_count).await?;
        }
        SourceKind::Xmltv => {
            checkpoint_reconciliation(control, result, "reconciling-epg", 0).await?;
            let mapping_stats = catalog
                .reconcile_epg_mappings()
                .await
                .map_err(|_| RefreshError::Catalog)?;
            checkpoint_reconciliation(
                control,
                result,
                "reconciling-complete",
                u64::try_from(mapping_stats.mappings_applied.max(0)).unwrap_or(u64::MAX),
            )
            .await?;
        }
        SourceKind::NetworkTuner => {}
    }
    Ok(())
}

fn reconciliation_batch_size() -> i64 {
    env::var("IPTV_RECONCILIATION_BATCH_SIZE")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_RECONCILIATION_BATCH_SIZE)
        .clamp(1, 10_000)
}

/// Returns the bounded number of durable provider reconciliation partitions.
///
/// Three partitions use three workers by default. The number controls only
/// candidate preparation. One final transaction still publishes the catalog.
fn reconciliation_partition_count() -> i32 {
    env::var("IPTV_RECONCILIATION_PARTITIONS")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(3)
        .clamp(1, 64)
}

async fn checkpoint_reconciliation(
    control: &impl JobControl,
    result: &IngestResult,
    phase: &str,
    records_prepared: u64,
) -> std::result::Result<(), RefreshError> {
    control
        .checkpoint(&IngestProgress {
            phase: phase.to_owned(),
            downloaded_bytes: result.downloaded_bytes,
            decoded_bytes: result.decoded_bytes,
            records_seen: result.records,
            records_prepared,
        })
        .await
        .map_err(RefreshError::Ingest)
}

fn source_id_from_payload(payload: &serde_json::Value) -> Option<Uuid> {
    payload
        .get("sourceId")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn refresh_target(
    kind: SourceKind,
    source_id: Uuid,
) -> std::result::Result<(SnapshotOwner, IngestFormat), RefreshError> {
    match kind {
        SourceKind::M3u => Ok((SnapshotOwner::ProviderAccount(source_id), IngestFormat::M3u)),
        SourceKind::Xmltv => Ok((SnapshotOwner::EpgSource(source_id), IngestFormat::Xmltv)),
        SourceKind::Xtream => Ok((
            SnapshotOwner::ProviderAccount(source_id),
            IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
        )),
        SourceKind::NetworkTuner => Err(RefreshError::UnsupportedSource),
    }
}

#[derive(Debug)]
enum RefreshError {
    InvalidPayload,
    SourceLoad,
    InvalidEndpoint,
    UnsupportedSource,
    Catalog,
    Ingest(IngestError),
}

impl RefreshError {
    const fn persisted_summary(&self) -> &'static str {
        match self {
            Self::InvalidPayload => "refresh job payload is invalid",
            Self::SourceLoad => "source configuration could not be loaded",
            Self::InvalidEndpoint => "source endpoint is invalid",
            Self::UnsupportedSource => "source type is not supported by the refresh worker",
            Self::Catalog => "source catalog reconciliation failed",
            Self::Ingest(error) => error.persisted_summary(),
        }
    }
}

#[derive(Debug)]
struct RefreshExecution {
    result: IngestResult,
    deferred: bool,
}

/// Stores provider snapshots for worker reconciliation before public publication.
///
/// The finalizer owns the only path that changes active snapshots and channels.
#[derive(Clone, Debug)]
struct StagedSnapshotActivator {
    snapshots: PgSnapshotStore,
}

impl StagedSnapshotActivator {
    fn new(snapshots: PgSnapshotStore) -> Self {
        Self { snapshots }
    }
}

impl SnapshotActivator for StagedSnapshotActivator {
    async fn activate(
        &self,
        snapshot: &iptv_ingest::PreparedSnapshot,
    ) -> std::result::Result<Uuid, IngestError> {
        self.snapshots.stage(snapshot).await
    }

    async fn activate_with_progress(
        &self,
        snapshot: &iptv_ingest::PreparedSnapshot,
        progress: &dyn ActivationProgress,
    ) -> std::result::Result<Uuid, IngestError> {
        self.snapshots.stage_with_progress(snapshot, progress).await
    }

    fn completion_phase(&self) -> &'static str {
        "staged"
    }
}

#[derive(Clone, Debug)]
struct WorkerJobControl {
    repository: JobRepository,
    job_id: Uuid,
    worker_id: String,
}

impl JobControl for WorkerJobControl {
    async fn checkpoint(&self, progress: &IngestProgress) -> std::result::Result<(), IngestError> {
        if self
            .repository
            .is_cancelled(self.job_id)
            .await
            .map_err(|_| IngestError::OwnershipLost)?
        {
            return Err(IngestError::Cancelled);
        }
        let progress = refresh_progress_json(progress);
        self.repository
            .heartbeat(self.job_id, &self.worker_id, &progress)
            .await
            .map_err(|_| IngestError::OwnershipLost)
    }

    async fn begin_activation(&self) -> std::result::Result<(), IngestError> {
        match self
            .repository
            .begin_activation(self.job_id, &self.worker_id)
            .await
        {
            Ok(()) => Ok(()),
            Err(_)
                if self
                    .repository
                    .is_cancelled(self.job_id)
                    .await
                    .unwrap_or(false) =>
            {
                Err(IngestError::Cancelled)
            }
            Err(_) => Err(IngestError::OwnershipLost),
        }
    }
}

impl WorkerJobControl {
    async fn checkpoint_after_activation(
        &self,
        progress: &IngestProgress,
    ) -> std::result::Result<(), iptv_persistence::PersistenceError> {
        let progress = refresh_progress_json(progress);
        self.repository
            .heartbeat(self.job_id, &self.worker_id, &progress)
            .await
    }
}

#[derive(Debug)]
struct WorkerProviderReconciliationProgress<'a> {
    control: &'a WorkerJobControl,
    result: &'a IngestResult,
}

impl ProviderReconciliationProgress for WorkerProviderReconciliationProgress<'_> {
    fn checkpoint(
        &self,
        update: ProviderReconciliationUpdate,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<(), iptv_persistence::PersistenceError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let phase = match update.phase {
                ProviderReconciliationPhase::Snapshot => "reconciling-snapshot",
                ProviderReconciliationPhase::Channels => "reconciling-channel-batch",
                ProviderReconciliationPhase::StreamLinks => "reconciling-link-batch",
                ProviderReconciliationPhase::Orphans => "reconciling-orphan-batch",
                ProviderReconciliationPhase::Revision => "reconciling-revision",
            };
            self.control
                .checkpoint_after_activation(&IngestProgress {
                    phase: phase.to_owned(),
                    downloaded_bytes: self.result.downloaded_bytes,
                    decoded_bytes: self.result.decoded_bytes,
                    records_seen: update.keys_total,
                    records_prepared: update.keys_completed,
                })
                .await
        })
    }
}

#[derive(Debug)]
struct WorkerStagingProgress<'a> {
    control: &'a WorkerJobControl,
    downloaded_bytes: u64,
    decoded_bytes: u64,
    records_seen: u64,
}

impl ActivationProgress for WorkerStagingProgress<'_> {
    fn checkpoint(
        &self,
        records_staged: u64,
        _records_total: u64,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), IngestError>> + '_>> {
        Box::pin(async move {
            self.control
                .checkpoint(&IngestProgress {
                    phase: "staging".to_owned(),
                    downloaded_bytes: self.downloaded_bytes,
                    decoded_bytes: self.decoded_bytes,
                    records_seen: self.records_seen,
                    records_prepared: records_staged,
                })
                .await
        })
    }
}

/// Maps an ingest phase to the stage-based progress JSON reported through
/// `JobRepository::heartbeat`. The stage names align with the public
/// `GET /api/v1/sources/{source_id}/sync-status` contract.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
#[allow(clippy::too_many_lines)]
fn refresh_progress_json(progress: &IngestProgress) -> serde_json::Value {
    let (stage, percent, records_processed, message): (&'static str, u8, u64, String) =
        match progress.phase.as_str() {
            "downloading" => {
                // Scale the percent from 1 to 14 based on downloaded bytes so the
                // UI shows progress during long downloads even without a
                // content-length header. 1 MB maps to ~3%, 50 MB to ~12%.
                let scaled = if progress.downloaded_bytes == 0 {
                    1_u8
                } else {
                    let mb = (progress.downloaded_bytes as f64) / 1_048_576.0;
                    let log_mb = mb.log2().max(0.0);
                    let raw = 1.0 + log_mb * 1.5;
                    raw.clamp(1.0, 14.0) as u8
                };
                (
                    "downloading",
                    scaled,
                    progress.records_seen,
                    "Downloading source data".to_owned(),
                )
            }
            "downloaded" => (
                "downloading",
                15,
                progress.records_seen,
                "Downloaded source data".to_owned(),
            ),
            "decoded" => (
                "parsing",
                30,
                progress.records_seen,
                "Decoded source artifact".to_owned(),
            ),
            "parsed" => (
                "parsing",
                45,
                progress.records_seen,
                "Parsed source records".to_owned(),
            ),
            "staging" => {
                let total = progress.records_seen;
                let staged = progress.records_prepared.min(total);
                let completed = staged.saturating_mul(15).checked_div(total).unwrap_or(0);
                let percent = 55 + u8::try_from(completed.min(15)).unwrap_or(15);
                (
                    "parsing",
                    percent,
                    staged,
                    format!("Staged {staged} of {total} source records"),
                )
            }
            "activated" => (
                "activating",
                70,
                progress.records_seen,
                "Activated snapshot".to_owned(),
            ),
            "staged" => (
                "reconciling",
                70,
                progress.records_seen,
                "Staged snapshot for worker reconciliation".to_owned(),
            ),
            "reconciling-partitions" => (
                "reconciling",
                85,
                progress.records_prepared,
                "Queued provider reconciliation partitions".to_owned(),
            ),
            "reconciling-channels" => (
                "reconciling",
                85,
                progress.records_prepared,
                "Reconciling channels".to_owned(),
            ),
            "reconciling-snapshot" => (
                "reconciling",
                86,
                progress.records_prepared,
                format!(
                    "Captured reconciliation state for {} source streams",
                    progress.records_seen
                ),
            ),
            "reconciling-channel-batch" => (
                "reconciling",
                reconciliation_batch_percent(
                    86,
                    90,
                    progress.records_prepared,
                    progress.records_seen,
                ),
                progress.records_prepared,
                format!(
                    "Reconciled {} of {} canonical channels",
                    progress.records_prepared, progress.records_seen
                ),
            ),
            "reconciling-link-batch" => (
                "reconciling",
                reconciliation_batch_percent(
                    91,
                    94,
                    progress.records_prepared,
                    progress.records_seen,
                ),
                progress.records_prepared,
                format!(
                    "Reconciled stream links for {} of {} canonical channels",
                    progress.records_prepared, progress.records_seen
                ),
            ),
            "reconciling-orphan-batch" => (
                "reconciling",
                95,
                progress.records_prepared,
                format!(
                    "Orphan query affected {} rows from {} source streams",
                    progress.records_prepared, progress.records_seen
                ),
            ),
            "reconciling-revision" => (
                "reconciling",
                96,
                progress.records_prepared,
                format!(
                    "Recorded reconciliation revision for {} source streams",
                    progress.records_seen
                ),
            ),
            "reconciling-epg" => (
                "reconciling",
                97,
                progress.records_prepared,
                format!("Reconciled {} channels", progress.records_prepared),
            ),
            "reconciling-events" => (
                "reconciling",
                98,
                progress.records_prepared,
                format!("Reconciled {} guide mappings", progress.records_prepared),
            ),
            "reconciling-complete" => (
                "reconciling",
                99,
                progress.records_prepared,
                format!(
                    "Updated {} dynamic event channels",
                    progress.records_prepared
                ),
            ),
            _ => (
                "downloading",
                0,
                progress.records_seen,
                "Refreshing source".to_owned(),
            ),
        };
    serde_json::json!({
        "stage": stage,
        "percent": percent,
        "bytesDownloaded": progress.downloaded_bytes,
        "recordsProcessed": records_processed,
        "message": message,
    })
}

fn reconciliation_batch_percent(start: u8, end: u8, completed: u64, total: u64) -> u8 {
    let span = u64::from(end.saturating_sub(start));
    let increment = completed
        .saturating_mul(span)
        .checked_div(total)
        .unwrap_or(0);
    start.saturating_add(u8::try_from(increment.min(span)).unwrap_or(u8::MAX))
}

#[derive(Clone, Debug)]
struct StreamEndpointProtector {
    master_key: MasterKey,
    associated_data: String,
}

impl EndpointProtector for StreamEndpointProtector {
    fn protect(&self, endpoint: &Url) -> std::result::Result<ProtectedEndpoint, IngestError> {
        let secret_ciphertext = self
            .master_key
            .encrypt_secret(
                endpoint.as_str().as_bytes(),
                self.associated_data.as_bytes(),
            )
            .map_err(|_| IngestError::EndpointProtection)?;
        Ok(ProtectedEndpoint {
            template: protected_endpoint_template(endpoint),
            secret_ciphertext: Some(secret_ciphertext),
        })
    }
}

fn protected_endpoint_template(endpoint: &Url) -> String {
    let Some(host) = endpoint.host_str() else {
        return format!("{}:[encrypted]", endpoint.scheme());
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = endpoint
        .port()
        .map_or_else(String::new, |port| format!(":{port}"));
    format!("{}://{host}{port}/[encrypted]", endpoint.scheme())
}

async fn runtime_versions() -> RuntimeVersions {
    RuntimeVersions {
        gateway: env!("CARGO_PKG_VERSION").to_owned(),
        git_hash: env!("GIT_COMMIT_HASH").to_owned(),
        git_tag: env!("GIT_TAG").to_owned(),
        ffmpeg: command_version("ffmpeg", &["-version"]).await,
        vlc: command_version("vlc", &["--version"]).await,
    }
}

async fn command_version(command: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(command)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::to_owned)
}

fn required_env(name: &str) -> Result<String> {
    required_env_from(&mut |name| env::var(name), name)
}

fn required_env_from<F>(lookup: &mut F, name: &str) -> Result<String>
where
    F: FnMut(&str) -> std::result::Result<String, env::VarError>,
{
    lookup(name).with_context(|| format!("required environment variable {name} is not set"))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Query, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{delete, get, post},
    };
    use iptv_media::StreamProbeQuality;
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU8, AtomicUsize, Ordering},
        },
    };

    const MASTER_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    const OUTPUT_TOKEN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const BOOTSTRAP_TOKEN: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn metrics_activity_check_is_conservative_and_aggregate() {
        assert!(metrics_report_active_sessions(
            "iptv_media_provider_active_sessions 1\n"
        ));
        assert!(!metrics_report_active_sessions(
            "iptv_media_provider_active_sessions 0\n"
        ));
        assert!(!metrics_report_active_sessions("not prometheus data\n"));
    }

    #[test]
    fn core_probe_config_requires_both_internal_credentials() {
        assert_eq!(
            core_probe_config_from(Some("http://core".to_owned()), Some("token".to_owned())),
            Some(("http://core".to_owned(), "token".to_owned()))
        );
        assert!(core_probe_config_from(Some("http://core".to_owned()), None).is_none());
        assert!(core_probe_config_from(None, Some("token".to_owned())).is_none());
        assert!(core_probe_config_from(Some(" ".to_owned()), Some("token".to_owned())).is_none());
        assert!(
            core_probe_config_from(Some("http://core".to_owned()), Some(" ".to_owned())).is_none()
        );
    }

    #[test]
    fn core_probe_config_reads_process_environment_safely() {
        // The test environment can omit either value. The wrapper must still
        // return a coherent optional configuration without exposing secrets.
        if let Some((url, token)) = core_probe_config() {
            assert!(!url.is_empty());
            assert!(!token.is_empty());
        }
    }

    #[tokio::test]
    async fn core_probe_reservation_treats_capacity_conflict_as_skipped() {
        let app = Router::new().route(
            "/internal/v1/provider-reservations",
            post(|| async { StatusCode::CONFLICT }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let result = reserve_core_probe_slot(
            &format!("http://{address}/"),
            "worker-token",
            &Uuid::now_v7(),
        )
        .await
        .expect("capacity conflict is a normal skip");
        assert!(result.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn core_probe_reservation_rejects_failed_or_malformed_core_responses() {
        for malformed in [false, true] {
            let app = Router::new().route(
                "/internal/v1/provider-reservations",
                post(move || async move {
                    if malformed {
                        (StatusCode::OK, "not-json").into_response()
                    } else {
                        StatusCode::BAD_GATEWAY.into_response()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            let result = reserve_core_probe_slot(
                &format!("http://{address}"),
                "worker-token",
                &Uuid::now_v7(),
            )
            .await;
            assert!(result.is_err());
            server.abort();
        }
    }

    #[tokio::test]
    async fn core_probe_reservation_release_calls_core_delete_endpoint() {
        let release_count = Arc::new(AtomicUsize::new(0));
        let release_count_for_route = Arc::clone(&release_count);
        let app = Router::new()
            .route(
                "/internal/v1/provider-reservations",
                post(|| async { Json(serde_json::json!({"reservation_id":"reservation-1"})) }),
            )
            .route(
                "/internal/v1/provider-reservations/{reservation_id}",
                delete(move || {
                    release_count_for_route.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::NO_CONTENT }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let reservation = reserve_core_probe_slot(
            &format!("http://{address}"),
            "worker-token",
            &Uuid::now_v7(),
        )
        .await
        .unwrap()
        .expect("core reservation");
        reservation.release().await;
        assert_eq!(release_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn core_active_session_check_reads_metrics_and_defers_when_core_is_unavailable() {
        let app = Router::new().route(
            "/metrics",
            get(|| async { "iptv_media_provider_active_sessions 2\n" }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::new();
        let active_url = format!("http://{address}/");
        assert!(core_has_active_sessions(&client, &active_url).await);
        assert!(should_defer_health_probes(&client, Some(&active_url)).await);
        server.abort();

        let invalid_body_app =
            Router::new().route("/metrics", get(|| async { vec![0xff_u8, 0xfe_u8] }));
        let invalid_body_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let invalid_body_address = invalid_body_listener.local_addr().unwrap();
        let invalid_body_server = tokio::spawn(async move {
            axum::serve(invalid_body_listener, invalid_body_app)
                .await
                .unwrap();
        });
        assert!(
            core_has_active_sessions(&client, &format!("http://{invalid_body_address}/")).await
        );
        invalid_body_server.abort();

        let broken_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broken_address = broken_listener.local_addr().unwrap();
        let broken_server = tokio::spawn(async move {
            let (mut socket, _) = broken_listener.accept().await.unwrap();
            let mut request = [0_u8; 256];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx",
            )
            .await
            .unwrap();
        });
        assert!(core_has_active_sessions(&client, &format!("http://{broken_address}/")).await);
        broken_server.await.unwrap();

        assert!(core_has_active_sessions(&client, "http://127.0.0.1:1").await);
        assert!(!should_defer_health_probes(&client, None).await);
        assert!(should_defer_health_probes(&client, Some("http://127.0.0.1:1")).await);
    }

    /// Computes the MPEG-2 CRC32 used by PSI sections. The polynomial is
    /// 0x04c11db7, which differs from the standard CRC32.
    fn mpeg_crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for &byte in bytes {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    #[derive(Default)]
    struct TestPostRefreshCatalog {
        calls: Mutex<Vec<&'static str>>,
        fail_at: Option<&'static str>,
    }

    impl TestPostRefreshCatalog {
        fn call(&self, name: &'static str) -> Result<(), iptv_persistence::PersistenceError> {
            self.calls.lock().unwrap().push(name);
            if self.fail_at == Some(name) {
                return Err(iptv_persistence::PersistenceError::InvalidSource(
                    "test hook failure".to_owned(),
                ));
            }
            Ok(())
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl PostRefreshCatalog for TestPostRefreshCatalog {
        fn reconcile_provider_account_with_progress<'a>(
            &'a self,
            _: Uuid,
            progress: &'a dyn ProviderReconciliationProgress,
            _batch_size: i64,
        ) -> ProviderReconcileFuture<'a> {
            Box::pin(async move {
                self.call("provider")?;
                for (phase, rows_affected) in [
                    (ProviderReconciliationPhase::Snapshot, 0),
                    (ProviderReconciliationPhase::Channels, 4),
                    (ProviderReconciliationPhase::StreamLinks, 6),
                    (ProviderReconciliationPhase::Orphans, 1),
                    (ProviderReconciliationPhase::Revision, 1),
                ] {
                    progress
                        .checkpoint(ProviderReconciliationUpdate {
                            phase,
                            rows_affected,
                            active_streams: 6,
                            keys_completed: match phase {
                                ProviderReconciliationPhase::Snapshot => 0,
                                _ => 2,
                            },
                            keys_total: 2,
                        })
                        .await?;
                }
                Ok(ReconcileStats {
                    channels: 4,
                    orphaned_channels_removed: 1,
                    stream_links: 6,
                })
            })
        }

        fn reconcile_epg_mappings(&self) -> EpgReconcileFuture<'_> {
            Box::pin(async move {
                self.call("epg")?;
                Ok(EpgMappingStats {
                    mappings_applied: 3,
                    mappings_removed: 0,
                    review_queued: 1,
                })
            })
        }

        fn scan_all_event_channels(&self) -> EventReconcileFuture<'_> {
            Box::pin(async move {
                self.call("events")?;
                Ok(2)
            })
        }
    }

    #[derive(Default)]
    struct RecordingJobControl {
        progress: Mutex<Vec<IngestProgress>>,
    }

    #[derive(Default)]
    struct RecordingProviderProgress {
        updates: Mutex<Vec<ProviderReconciliationUpdate>>,
    }

    impl ProviderReconciliationProgress for RecordingProviderProgress {
        fn checkpoint(
            &self,
            update: ProviderReconciliationUpdate,
        ) -> Pin<
            Box<
                dyn Future<Output = std::result::Result<(), iptv_persistence::PersistenceError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                self.updates.lock().expect("lock").push(update);
                Ok(())
            })
        }
    }

    impl JobControl for RecordingJobControl {
        async fn checkpoint(
            &self,
            progress: &IngestProgress,
        ) -> std::result::Result<(), IngestError> {
            self.progress.lock().expect("lock").push(progress.clone());
            Ok(())
        }
    }

    fn ingest_result() -> IngestResult {
        IngestResult {
            snapshot_id: Uuid::now_v7(),
            checksum_sha256: "0".repeat(64),
            downloaded_bytes: 12,
            decoded_bytes: 24,
            records: 100,
        }
    }

    const XTREAM_PHASE_INITIAL: u8 = 0;
    const XTREAM_PHASE_RENAMED: u8 = 1;
    const XTREAM_PHASE_AUTH_FAILURE: u8 = 2;
    const XTREAM_PHASE_EMPTY_STREAMS: u8 = 3;

    #[derive(Default)]
    struct XtreamFixtureState {
        phase: AtomicU8,
        authentication_requests: AtomicUsize,
        category_requests: AtomicUsize,
        stream_requests: AtomicUsize,
        short_epg_requests: AtomicUsize,
    }

    impl XtreamFixtureState {
        fn phase(&self) -> u8 {
            self.phase.load(Ordering::SeqCst)
        }

        fn set_phase(&self, phase: u8) {
            self.phase.store(phase, Ordering::SeqCst);
        }

        fn request_counts(&self) -> (usize, usize, usize) {
            (
                self.authentication_requests.load(Ordering::SeqCst),
                self.category_requests.load(Ordering::SeqCst),
                self.stream_requests.load(Ordering::SeqCst),
            )
        }

        fn short_epg_requests(&self) -> usize {
            self.short_epg_requests.load(Ordering::SeqCst)
        }
    }

    async fn xtream_fixture_response(
        State(state): State<Arc<XtreamFixtureState>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Response {
        let valid_credentials = query
            .get("username")
            .is_some_and(|value| value == "worker-user-canary")
            && query
                .get("password")
                .is_some_and(|value| value == "worker-password-canary");
        if !valid_credentials {
            return StatusCode::FORBIDDEN.into_response();
        }

        match query.get("action").map(String::as_str) {
            None => {
                state.authentication_requests.fetch_add(1, Ordering::SeqCst);
                let authenticated = state.phase() != XTREAM_PHASE_AUTH_FAILURE;
                Json(serde_json::json!({
                    "user_info": {
                        "auth": authenticated,
                        "status": if authenticated { "Active" } else { "Disabled" },
                        "username": "worker-user-canary",
                        "password": "worker-password-canary",
                    },
                }))
                .into_response()
            }
            Some("get_live_categories") => {
                state.category_requests.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!([
                    {"category_id": "10", "category_name": "Sports"}
                ]))
                .into_response()
            }
            Some("get_live_streams") => {
                state.stream_requests.fetch_add(1, Ordering::SeqCst);
                if state.phase() == XTREAM_PHASE_EMPTY_STREAMS {
                    return Json(serde_json::json!([])).into_response();
                }
                let name = if state.phase() == XTREAM_PHASE_RENAMED {
                    "Worker Sports 7 Updated"
                } else {
                    "Worker Sports 7"
                };
                Json(serde_json::json!([
                    {
                        "stream_id": 7,
                        "name": name,
                        "category_id": "10",
                        "epg_channel_id": "worker-epg-7",
                        "num": 7,
                        "stream_type": "live",
                        "access_token": "stream-access-token-canary",
                    }
                ]))
                .into_response()
            }
            Some("get_short_epg") => {
                state.short_epg_requests.fetch_add(1, Ordering::SeqCst);
                // The live-stream fixture exposes stream_id 7 with
                // epg_channel_id "worker-epg-7". Return one programme whose
                // channel_id matches that tvg-id so the catalog tvg-id
                // reconciliation pass links the short EPG channel to the
                // canonical channel that the live-stream snapshot produced.
                let start = chrono::Utc::now() - chrono::Duration::minutes(5);
                let end = chrono::Utc::now() + chrono::Duration::hours(3);
                Json(serde_json::json!({
                    "epg_listings": [
                        {
                            "id": "short-epg-1",
                            "epg_id": "worker-epg-7",
                            "title": "QnJvbmNvcyB2cyBDaGllZnM=",
                            "description": "TGl2ZSBmcm9tIERlbnZlcg==",
                            "lang": "en",
                            "start": start.to_rfc3339(),
                            "end": end.to_rfc3339(),
                            "channel_id": "worker-epg-7",
                            "event_id": "short-epg-game-1"
                        }
                    ]
                }))
                .into_response()
            }
            Some(_) => StatusCode::BAD_REQUEST.into_response(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_refresh_job(
        pool: &sqlx::PgPool,
        jobs: &JobRepository,
        sources: &SourceRepository,
        snapshots: &PgSnapshotStore,
        catalog: &CatalogRepository,
        master_key: &MasterKey,
        worker_id: &str,
        job_id: Uuid,
    ) {
        sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
            .bind(job_id)
            .execute(pool)
            .await
            .expect("set job max attempts");
        force_running(pool, job_id, worker_id).await;
        let job = fetch_job(pool, job_id).await;
        process_job(
            jobs, sources, snapshots, catalog, master_key, worker_id, job,
        )
        .await
        .expect("process refresh job");
        // Source refresh now only stages provider data. Drive its child jobs
        // in this helper so existing end-to-end tests assert final public
        // state without changing production worker scheduling.
        loop {
            let child: Option<JobRecord> = sqlx::query_as(
                r"
                SELECT *
                FROM jobs
                WHERE status = 'queued'
                  AND kind IN (
                      'reconcile-provider-partition',
                      'finalize-provider-reconciliation'
                  )
                ORDER BY created_at, id
                LIMIT 1
                ",
            )
            .fetch_optional(pool)
            .await
            .expect("fetch reconciliation child job");
            let Some(child) = child else {
                break;
            };
            force_running(pool, child.id, worker_id).await;
            let child = fetch_job(pool, child.id).await;
            process_job(
                jobs, sources, snapshots, catalog, master_key, worker_id, child,
            )
            .await
            .expect("process reconciliation child job");
        }
    }

    #[tokio::test]
    async fn m3u_post_refresh_scans_events_after_reconciliation() {
        let catalog = TestPostRefreshCatalog::default();
        let control = RecordingJobControl::default();
        let provider_progress = RecordingProviderProgress::default();
        run_post_refresh_catalog(
            SourceKind::M3u,
            Uuid::now_v7(),
            &catalog,
            &control,
            &ingest_result(),
            &provider_progress,
            DEFAULT_RECONCILIATION_BATCH_SIZE,
        )
        .await
        .expect("post-refresh work");
        assert_eq!(catalog.calls(), ["provider", "epg", "events"]);
        assert_eq!(
            provider_progress.updates.lock().expect("lock").as_slice(),
            [
                ProviderReconciliationUpdate {
                    phase: ProviderReconciliationPhase::Snapshot,
                    rows_affected: 0,
                    active_streams: 6,
                    keys_completed: 0,
                    keys_total: 2,
                },
                ProviderReconciliationUpdate {
                    phase: ProviderReconciliationPhase::Channels,
                    rows_affected: 4,
                    active_streams: 6,
                    keys_completed: 2,
                    keys_total: 2,
                },
                ProviderReconciliationUpdate {
                    phase: ProviderReconciliationPhase::StreamLinks,
                    rows_affected: 6,
                    active_streams: 6,
                    keys_completed: 2,
                    keys_total: 2,
                },
                ProviderReconciliationUpdate {
                    phase: ProviderReconciliationPhase::Orphans,
                    rows_affected: 1,
                    active_streams: 6,
                    keys_completed: 2,
                    keys_total: 2,
                },
                ProviderReconciliationUpdate {
                    phase: ProviderReconciliationPhase::Revision,
                    rows_affected: 1,
                    active_streams: 6,
                    keys_completed: 2,
                    keys_total: 2,
                },
            ]
        );
        let progress = control
            .progress
            .lock()
            .expect("lock")
            .iter()
            .map(|entry| (entry.phase.clone(), entry.records_prepared))
            .collect::<Vec<_>>();
        assert_eq!(
            progress,
            [
                ("reconciling-channels".to_owned(), 0),
                ("reconciling-epg".to_owned(), 4),
                ("reconciling-events".to_owned(), 3),
                ("reconciling-complete".to_owned(), 2),
            ]
        );

        let xtream = TestPostRefreshCatalog::default();
        let control = RecordingJobControl::default();
        let provider_progress = RecordingProviderProgress::default();
        run_post_refresh_catalog(
            SourceKind::Xtream,
            Uuid::now_v7(),
            &xtream,
            &control,
            &ingest_result(),
            &provider_progress,
            DEFAULT_RECONCILIATION_BATCH_SIZE,
        )
        .await
        .expect("Xtream post-refresh work");
        assert_eq!(xtream.calls(), ["provider", "epg", "events"]);
    }

    #[tokio::test]
    async fn failed_or_non_m3u_post_refresh_never_scans_events() {
        let failed = TestPostRefreshCatalog {
            fail_at: Some("provider"),
            ..TestPostRefreshCatalog::default()
        };
        let failed_control = RecordingJobControl::default();
        let failed_provider_progress = RecordingProviderProgress::default();
        assert!(
            run_post_refresh_catalog(
                SourceKind::M3u,
                Uuid::now_v7(),
                &failed,
                &failed_control,
                &ingest_result(),
                &failed_provider_progress,
                DEFAULT_RECONCILIATION_BATCH_SIZE,
            )
            .await
            .is_err()
        );
        assert_eq!(failed.calls(), ["provider"]);

        let xmltv = TestPostRefreshCatalog::default();
        let control = RecordingJobControl::default();
        let provider_progress = RecordingProviderProgress::default();
        run_post_refresh_catalog(
            SourceKind::Xmltv,
            Uuid::now_v7(),
            &xmltv,
            &control,
            &ingest_result(),
            &provider_progress,
            DEFAULT_RECONCILIATION_BATCH_SIZE,
        )
        .await
        .expect("XMLTV post-refresh work");
        assert_eq!(xmltv.calls(), ["epg"]);
    }

    fn lookup(
        entries: &[(&str, &str)],
    ) -> impl FnMut(&str) -> std::result::Result<String, env::VarError> {
        let values = entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        move |name| values.get(name).cloned().ok_or(env::VarError::NotPresent)
    }

    fn required_settings() -> [(&'static str, &'static str); 4] {
        [
            ("IPTV_ADMIN_PASSWORD_HASH", "already-hashed"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]
    }

    #[test]
    fn command_defaults_to_serve_and_accepts_every_supported_mode() {
        assert_eq!(parse_command(None).expect("default"), GatewayCommand::Serve);
        assert_eq!(
            parse_command(Some("serve")).expect("serve"),
            GatewayCommand::Serve
        );
        assert_eq!(
            parse_command(Some("worker")).expect("worker"),
            GatewayCommand::Worker
        );
        assert_eq!(
            parse_command(Some("versions")).expect("versions"),
            GatewayCommand::Versions
        );
    }

    #[test]
    fn command_rejects_unknown_or_empty_values() {
        for argument in ["", "Serve", "other"] {
            let error = parse_command(Some(argument)).expect_err("unknown command");
            assert!(
                error
                    .to_string()
                    .contains("expected serve, worker, or versions")
            );
            assert!(error.to_string().contains(&format!("{argument:?}")));
        }
    }

    #[test]
    fn tracing_initialization_accepts_the_default_filter() {
        init_tracing();
    }

    #[test]
    fn server_environment_uses_stable_defaults() {
        let environment = server_environment_from(lookup(&required_settings())).expect("settings");
        assert_eq!(environment.bind, "0.0.0.0:8081".parse().expect("address"));
        assert_eq!(environment.public_base_url, "http://localhost:8080");
        assert_eq!(environment.output_token, OUTPUT_TOKEN);
        assert_eq!(environment.admin_bootstrap_token, BOOTSTRAP_TOKEN);
        assert_eq!(environment.admin_password_hash, "already-hashed");
        assert_eq!(environment.tuner_count, 1);
    }

    #[test]
    fn server_environment_accepts_all_overrides() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_BIND", "127.0.0.1:9000"),
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_PUBLIC_BASE_URL", "https://iptv.example.test/base"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
            ("IPTV_TUNER_COUNT", "7"),
        ]))
        .expect("settings");
        assert_eq!(environment.bind, "127.0.0.1:9000".parse().expect("address"));
        assert_eq!(
            environment.public_base_url,
            "https://iptv.example.test/base"
        );
        assert_eq!(environment.tuner_count, 7);
    }

    #[test]
    fn invalid_tuner_count_preserves_the_original_defaulting_behavior() {
        for value in ["", "not-a-number", "65536", "-1"] {
            let environment = server_environment_from(lookup(&[
                ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
                ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
                ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
                ("IPTV_MASTER_KEY", MASTER_KEY),
                ("IPTV_TUNER_COUNT", value),
            ]))
            .expect("invalid tuner count defaults");
            assert_eq!(environment.tuner_count, 1);
        }
    }

    #[test]
    fn zero_tuners_uses_the_supported_default() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
            ("IPTV_TUNER_COUNT", "0"),
        ]))
        .expect("zero tuner count defaults");
        assert_eq!(environment.tuner_count, 1);
    }

    #[test]
    fn plaintext_password_is_hashed_when_precomputed_hash_is_absent_or_blank() {
        for optional_hash in [None, Some("  ")] {
            let mut entries = vec![
                ("IPTV_ADMIN_PASSWORD", "correct horse battery staple"),
                ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
                ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
                ("IPTV_MASTER_KEY", MASTER_KEY),
            ];
            if let Some(hash) = optional_hash {
                entries.push(("IPTV_ADMIN_PASSWORD_HASH", hash));
            }
            let environment = server_environment_from(lookup(&entries)).expect("hash password");
            assert_ne!(
                environment.admin_password_hash,
                "correct horse battery staple"
            );
            assert!(environment.admin_password_hash.starts_with("$argon2"));
        }
    }

    #[test]
    fn invalid_bind_and_missing_secrets_have_contextual_errors() {
        let bind_error = server_environment_from(lookup(&[
            ("IPTV_BIND", "not-an-address"),
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect_err("invalid bind");
        assert!(bind_error.to_string().contains("parse IPTV_BIND"));

        for missing in [
            "IPTV_OUTPUT_TOKEN",
            "IPTV_ADMIN_BOOTSTRAP_TOKEN",
            "IPTV_MASTER_KEY",
        ] {
            let entries = required_settings()
                .into_iter()
                .filter(|(name, _)| *name != missing)
                .collect::<Vec<_>>();
            let error = server_environment_from(lookup(&entries)).expect_err("missing secret");
            assert!(error.to_string().contains(missing));
        }
    }

    #[test]
    fn malformed_master_keys_fail_closed_without_echoing_the_value() {
        let invalid = "not-a-valid-master-key";
        let error = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", invalid),
        ]))
        .expect_err("invalid master key");
        assert!(error.to_string().contains("parse IPTV_MASTER_KEY"));
        assert!(!error.to_string().contains(invalid));
    }

    #[test]
    fn missing_plaintext_password_is_reported_when_hash_is_unavailable() {
        let error = server_environment_from(lookup(&[
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
        ]))
        .expect_err("missing password");
        assert!(error.to_string().contains("IPTV_ADMIN_PASSWORD"));
    }

    #[test]
    fn environment_converts_to_api_config_without_data_loss() {
        let environment = server_environment_from(lookup(&required_settings())).expect("settings");
        let versions = RuntimeVersions {
            gateway: "gateway-version".to_owned(),
            git_hash: "abc1234".to_owned(),
            git_tag: "v0.1.0".to_owned(),
            ffmpeg: Some("ffmpeg-version".to_owned()),
            vlc: Some("vlc-version".to_owned()),
        };
        let config = environment.into_config(versions);
        assert_eq!(config.public_base_url, "http://localhost:8080");
        assert_eq!(config.output_token, OUTPUT_TOKEN);
        assert_eq!(config.admin_bootstrap_token, BOOTSTRAP_TOKEN);
        assert_eq!(config.admin_password_hash, "already-hashed");
        assert_eq!(config.tuner_count, 1);
        assert_eq!(config.runtime_versions.gateway, "gateway-version");
        assert_eq!(
            config.runtime_versions.ffmpeg.as_deref(),
            Some("ffmpeg-version")
        );
        assert_eq!(config.runtime_versions.vlc.as_deref(), Some("vlc-version"));
    }

    #[test]
    fn server_environment_debug_redacts_all_plaintext_secrets() {
        let environment = server_environment_from(lookup(&required_settings())).expect("settings");
        let debug = format!("{environment:?}");
        assert!(debug.contains("0.0.0.0:8081"));
        assert!(debug.contains("<redacted>"));
        for secret in [OUTPUT_TOKEN, BOOTSTRAP_TOKEN, "already-hashed"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn worker_id_uses_override_or_default() {
        assert_eq!(worker_id_from(lookup(&[])), "worker-1");
        assert_eq!(
            worker_id_from(lookup(&[("IPTV_WORKER_ID", "worker-blue")])),
            "worker-blue"
        );
        assert_eq!(
            worker_id_from(lookup(&[("HOSTNAME", "compose-worker-3")])),
            "compose-worker-3"
        );
    }

    #[test]
    fn refresh_targets_match_source_ownership() {
        let source_id = Uuid::now_v7();
        assert_eq!(
            refresh_target(SourceKind::M3u, source_id).expect("M3U target"),
            (SnapshotOwner::ProviderAccount(source_id), IngestFormat::M3u)
        );
        assert_eq!(
            refresh_target(SourceKind::Xmltv, source_id).expect("XMLTV target"),
            (SnapshotOwner::EpgSource(source_id), IngestFormat::Xmltv)
        );
        assert_eq!(
            refresh_target(SourceKind::Xtream, source_id).expect("Xtream target"),
            (
                SnapshotOwner::ProviderAccount(source_id),
                IngestFormat::Xtream(XtreamPayloadKind::LiveStreams)
            )
        );
        assert!(matches!(
            refresh_target(SourceKind::NetworkTuner, source_id),
            Err(RefreshError::UnsupportedSource)
        ));
    }

    #[test]
    fn refresh_payload_requires_a_valid_source_id() {
        let source_id = Uuid::now_v7();
        assert_eq!(
            source_id_from_payload(&serde_json::json!({"sourceId": source_id})),
            Some(source_id)
        );
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"sourceId": null}),
            serde_json::json!({"sourceId": "invalid"}),
        ] {
            assert_eq!(source_id_from_payload(&payload), None);
        }
    }

    #[test]
    fn health_probe_payload_requires_a_valid_provider_stream_id() {
        let stream_id = Uuid::now_v7();
        assert_eq!(
            provider_stream_id_from_payload(&serde_json::json!({
                "providerStreamId": stream_id
            })),
            Some(stream_id)
        );
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"providerStreamId": null}),
            serde_json::json!({"providerStreamId": "invalid"}),
        ] {
            assert_eq!(provider_stream_id_from_payload(&payload), None);
        }
    }

    #[test]
    fn redacted_failure_messages_omit_urls_and_credentials() {
        for failure in [
            StreamProbeFailure::CapacityFull,
            StreamProbeFailure::StartupTimeout,
            StreamProbeFailure::Http,
            StreamProbeFailure::HttpStatus,
            StreamProbeFailure::NoPat,
            StreamProbeFailure::NoPmt,
            StreamProbeFailure::NoVideo,
            StreamProbeFailure::NoAudio,
            StreamProbeFailure::InsufficientPackets {
                seen: 3,
                required: 32,
            },
            StreamProbeFailure::MalformedTransport,
        ] {
            let message = redacted_failure_message(&failure);
            assert!(!message.contains("http"));
            assert!(!message.contains("://"));
            assert!(!message.contains("token"));
            assert!(!message.is_empty());
        }
    }

    #[test]
    fn stream_health_update_for_alive_carries_quality_data() {
        let stream_id = Uuid::now_v7();
        let outcome = StreamProbeOutcome::Alive {
            quality: StreamProbeQuality {
                video_codec: Some("h264".to_owned()),
                audio_codec: Some("mp2".to_owned()),
            },
            packets_seen: 64,
            duration_ms: 1_200,
        };
        let update = stream_health_update_for(stream_id, &outcome)
            .expect("alive outcome produces a health update");
        assert_eq!(update.provider_stream_id, stream_id);
        assert_eq!(update.status, "alive");
        assert_eq!(update.video_codec.as_deref(), Some("h264"));
        assert_eq!(update.audio_codec.as_deref(), Some("mp2"));
        assert_eq!(update.check_duration_ms, Some(1_200));
        assert!(update.error.is_none());
    }

    #[test]
    fn stream_health_update_for_dead_carries_redacted_error() {
        let stream_id = Uuid::now_v7();
        let outcome = StreamProbeOutcome::Dead(StreamProbeFailure::NoPmt);
        let update = stream_health_update_for(stream_id, &outcome)
            .expect("dead outcome produces a health update");
        assert_eq!(update.status, "dead");
        assert_eq!(update.error.as_deref(), Some("no PMT section observed"));
    }

    #[test]
    fn stream_health_update_for_skipped_returns_none() {
        let stream_id = Uuid::now_v7();
        let outcome = StreamProbeOutcome::Skipped;
        let update = stream_health_update_for(stream_id, &outcome);
        // A skipped probe must not persist an update so the
        // stream_health_checks.status check constraint is not violated and the
        // known-good health status and ranking are preserved.
        assert!(update.is_none());
    }

    #[test]
    fn stream_endpoint_protection_removes_all_url_credentials() {
        let protector = StreamEndpointProtector {
            master_key: MasterKey::from_bytes([9_u8; 32]),
            associated_data: "iptv-provider-stream:v1:test".to_owned(),
        };
        let endpoint = Url::parse(
            "https://alice:password@provider.test:8443/live/alice/password/1.ts?token=secret",
        )
        .expect("URL");
        let first = protector.protect(&endpoint).expect("protect");
        let second = protector.protect(&endpoint).expect("protect again");
        assert_eq!(first.template, "https://provider.test:8443/[encrypted]");
        assert_ne!(first.secret_ciphertext, second.secret_ciphertext);
        let serialized = serde_json::to_string(&first).expect("serialize");
        for secret in ["alice", "password@", "token=", "/secret"] {
            assert!(!serialized.contains(secret));
        }
    }

    #[test]
    fn protected_endpoint_templates_handle_ipv6_and_hostless_urls() {
        assert_eq!(
            protected_endpoint_template(&Url::parse("http://[::1]:8090/live").expect("IPv6 URL")),
            "http://[::1]:8090/[encrypted]"
        );
        assert_eq!(
            protected_endpoint_template(&Url::parse("data:text/plain,secret").expect("data URL")),
            "data:[encrypted]"
        );
    }

    #[test]
    fn refresh_errors_have_stable_safe_summaries() {
        assert_eq!(
            RefreshError::InvalidPayload.persisted_summary(),
            "refresh job payload is invalid"
        );
        assert_eq!(
            RefreshError::Ingest(IngestError::HttpStatus(401)).persisted_summary(),
            "HTTP source returned an unsuccessful status"
        );
    }

    #[test]
    fn required_lookup_preserves_error_context() {
        let mut missing = lookup(&[]);
        let error = required_env_from(&mut missing, "REQUIRED_VALUE").expect_err("missing");
        assert_eq!(
            error.to_string(),
            "required environment variable REQUIRED_VALUE is not set"
        );
    }

    #[tokio::test]
    async fn command_version_reads_only_the_first_stdout_line() {
        let version = command_version("/bin/sh", &["-c", "printf 'tool 1.2\\nignored\\n'"])
            .await
            .expect("version");
        assert_eq!(version, "tool 1.2");
    }

    #[tokio::test]
    async fn command_version_fails_closed() {
        assert_eq!(
            command_version("/bin/sh", &["-c", "printf failure; exit 3"]).await,
            None
        );
        assert_eq!(
            command_version("/definitely/missing/iptv-command", &[]).await,
            None
        );
        assert_eq!(command_version("/bin/sh", &["-c", ":"]).await, None);
    }

    #[tokio::test]
    async fn runtime_version_always_reports_the_gateway_build() {
        let versions = runtime_versions().await;
        assert_eq!(versions.gateway, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn ipv6_bind_is_supported() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_BIND", "[::1]:8081"),
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect("IPv6 bind");
        assert!(environment.bind.is_ipv6());
    }

    #[test]
    fn maximum_tuner_count_is_supported() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
            ("IPTV_TUNER_COUNT", "65535"),
        ]))
        .expect("maximum tuner count");
        assert_eq!(environment.tuner_count, u16::MAX);
    }

    #[test]
    fn configured_hash_is_preserved_byte_for_byte() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "  precomputed hash  "),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect("precomputed hash");
        assert_eq!(environment.admin_password_hash, "  precomputed hash  ");
    }

    #[test]
    fn explicitly_blank_public_url_is_preserved() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_PUBLIC_BASE_URL", ""),
            ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect("blank public URL");
        assert!(environment.public_base_url.is_empty());
    }

    #[test]
    fn blank_short_and_placeholder_tokens_fail_closed() {
        for (name, value) in [
            ("IPTV_OUTPUT_TOKEN", ""),
            ("IPTV_OUTPUT_TOKEN", "short"),
            (
                "IPTV_OUTPUT_TOKEN",
                "development-output-token-change-me-00000000000000000000000000000000",
            ),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", ""),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "short"),
            (
                "IPTV_ADMIN_BOOTSTRAP_TOKEN",
                "replace-with-bootstrap-token-00000000000000000000000000000000",
            ),
        ] {
            let mut entries = required_settings().to_vec();
            entries.retain(|(entry_name, _)| *entry_name != name);
            entries.push((name, value));
            let error = server_environment_from(lookup(&entries)).expect_err("invalid token");
            assert!(error.to_string().contains(name));
            if !value.is_empty() {
                assert!(!error.to_string().contains(value));
            }
        }
    }

    #[test]
    fn tokens_reject_characters_outside_hexadecimal_and_base64() {
        for (name, value) in [
            (
                "IPTV_OUTPUT_TOKEN",
                "!1111111111111111111111111111111111111111111111111111111111111111",
            ),
            (
                "IPTV_ADMIN_BOOTSTRAP_TOKEN",
                "?2222222222222222222222222222222222222222222222222222222222222222",
            ),
        ] {
            let mut entries = required_settings().to_vec();
            entries.retain(|(entry_name, _)| *entry_name != name);
            entries.push((name, value));
            let error = server_environment_from(lookup(&entries)).expect_err("invalid token");
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn blank_short_and_placeholder_passwords_fail_closed() {
        for password in [
            "",
            "short",
            "development-admin-password-change-me",
            " replace-this-password ",
        ] {
            let error = server_environment_from(lookup(&[
                ("IPTV_ADMIN_PASSWORD", password),
                ("IPTV_OUTPUT_TOKEN", OUTPUT_TOKEN),
                ("IPTV_ADMIN_BOOTSTRAP_TOKEN", BOOTSTRAP_TOKEN),
                ("IPTV_MASTER_KEY", MASTER_KEY),
            ]))
            .expect_err("invalid password");
            assert!(error.to_string().contains("IPTV_ADMIN_PASSWORD"));
            if !password.is_empty() {
                assert!(!error.to_string().contains(password));
            }
        }
    }

    #[test]
    fn blank_worker_id_falls_back_to_hostname_then_default() {
        assert_eq!(
            worker_id_from(lookup(&[("IPTV_WORKER_ID", "")])),
            "worker-1"
        );
        assert_eq!(
            worker_id_from(lookup(&[("IPTV_WORKER_ID", ""), ("HOSTNAME", "pod-abc")])),
            "pod-abc"
        );
    }

    #[test]
    fn required_lookup_returns_present_blank_value() {
        let mut values = lookup(&[("VALUE", "")]);
        assert_eq!(
            required_env_from(&mut values, "VALUE").expect("present"),
            ""
        );
    }

    #[test]
    fn lookup_helper_distinguishes_present_and_absent_values() {
        let mut values = lookup(&[("PRESENT", "value")]);
        assert_eq!(values("PRESENT").expect("present"), "value");
        assert!(matches!(values("ABSENT"), Err(env::VarError::NotPresent)));
    }

    #[test]
    fn environment_values_are_queried_in_startup_order() {
        let mut queried = Vec::new();
        let environment = server_environment_from(|name| {
            queried.push(name.to_owned());
            match name {
                "IPTV_ADMIN_PASSWORD_HASH" => Ok("hash".to_owned()),
                "IPTV_OUTPUT_TOKEN" => Ok(OUTPUT_TOKEN.to_owned()),
                "IPTV_ADMIN_BOOTSTRAP_TOKEN" => Ok(BOOTSTRAP_TOKEN.to_owned()),
                "IPTV_MASTER_KEY" => Ok(MASTER_KEY.to_owned()),
                _ => Err(env::VarError::NotPresent),
            }
        })
        .expect("settings");
        assert_eq!(environment.tuner_count, 1);
        assert_eq!(
            queried,
            [
                "IPTV_BIND",
                "IPTV_ADMIN_PASSWORD_HASH",
                "IPTV_PUBLIC_BASE_URL",
                "IPTV_OIDC_ISSUER_URL",
                "IPTV_OIDC_CLIENT_ID",
                "IPTV_OIDC_CLIENT_SECRET",
                "IPTV_OIDC_REDIRECT_URL",
                "IPTV_OIDC_ALLOWED_SUBJECTS",
                "IPTV_OIDC_ALLOWED_EMAILS",
                "IPTV_OIDC_SCOPES",
                "IPTV_OUTPUT_TOKEN",
                "IPTV_ADMIN_BOOTSTRAP_TOKEN",
                "IPTV_MASTER_KEY",
                "IPTV_TUNER_COUNT"
            ]
        );
    }

    #[test]
    fn invalid_bind_short_circuits_later_environment_queries() {
        let mut queried = Vec::new();
        let error = server_environment_from(|name| {
            queried.push(name.to_owned());
            Ok("invalid-address".to_owned())
        })
        .expect_err("invalid bind");
        assert!(error.to_string().contains("parse IPTV_BIND"));
        assert_eq!(queried, ["IPTV_BIND"]);
    }

    #[test]
    fn missing_password_short_circuits_secret_queries() {
        let mut queried = Vec::new();
        let error = server_environment_from(|name| {
            queried.push(name.to_owned());
            Err(env::VarError::NotPresent)
        })
        .expect_err("password required first");
        assert!(error.to_string().contains("IPTV_ADMIN_PASSWORD"));
        assert_eq!(
            queried,
            [
                "IPTV_BIND",
                "IPTV_ADMIN_PASSWORD_HASH",
                "IPTV_ADMIN_PASSWORD"
            ]
        );
    }

    #[test]
    fn missing_output_token_is_reported_before_bootstrap_token() {
        let mut queried = Vec::new();
        let error = server_environment_from(|name| {
            queried.push(name.to_owned());
            match name {
                "IPTV_ADMIN_PASSWORD_HASH" => Ok("hash".to_owned()),
                _ => Err(env::VarError::NotPresent),
            }
        })
        .expect_err("output token required first");
        assert!(error.to_string().contains("IPTV_OUTPUT_TOKEN"));
        assert!(
            !queried
                .iter()
                .any(|name| name == "IPTV_ADMIN_BOOTSTRAP_TOKEN")
        );
    }

    #[test]
    fn config_supports_unavailable_optional_runtime_tools() {
        let environment = server_environment_from(lookup(&required_settings())).expect("settings");
        let config = environment.into_config(RuntimeVersions {
            gateway: "1.0.0".to_owned(),
            git_hash: "unknown".to_owned(),
            git_tag: "unknown".to_owned(),
            ffmpeg: None,
            vlc: None,
        });
        assert_eq!(config.runtime_versions.gateway, "1.0.0");
        assert!(config.runtime_versions.ffmpeg.is_none());
        assert!(config.runtime_versions.vlc.is_none());
    }

    #[test]
    fn explicit_serve_command_is_case_sensitive() {
        assert_eq!(
            parse_command(Some("serve")).expect("serve"),
            GatewayCommand::Serve
        );
        assert!(parse_command(Some("SERVE")).is_err());
    }

    #[test]
    fn explicit_worker_command_is_case_sensitive() {
        assert_eq!(
            parse_command(Some("worker")).expect("worker"),
            GatewayCommand::Worker
        );
        assert!(parse_command(Some("Worker")).is_err());
    }

    #[test]
    fn explicit_versions_command_is_case_sensitive() {
        assert_eq!(
            parse_command(Some("versions")).expect("versions"),
            GatewayCommand::Versions
        );
        assert!(parse_command(Some("version")).is_err());
    }

    #[tokio::test]
    async fn command_version_preserves_leading_and_trailing_space() {
        let version = command_version("/bin/sh", &["-c", "printf '  tool 1.2  \\n'"])
            .await
            .expect("version");
        assert_eq!(version, "  tool 1.2  ");
    }

    #[tokio::test]
    async fn command_version_ignores_stderr_on_success() {
        let version = command_version(
            "/bin/sh",
            &["-c", "printf 'noise\\n' >&2; printf 'tool 2.0\\n'"],
        )
        .await
        .expect("stdout version");
        assert_eq!(version, "tool 2.0");
    }

    #[tokio::test]
    async fn command_version_accepts_arguments_containing_spaces() {
        let version = command_version("/bin/sh", &["-c", "printf '%s\\n' \"$1\"", "sh", "v 3.0"])
            .await
            .expect("spaced argument");
        assert_eq!(version, "v 3.0");
    }

    #[tokio::test]
    async fn command_version_handles_output_without_final_newline() {
        let version = command_version("/bin/sh", &["-c", "printf 'tool 4.0'"])
            .await
            .expect("version without newline");
        assert_eq!(version, "tool 4.0");
    }

    #[test]
    fn refresh_error_summaries_cover_source_endpoint_and_unsupported_variants() {
        assert_eq!(
            RefreshError::SourceLoad.persisted_summary(),
            "source configuration could not be loaded"
        );
        assert_eq!(
            RefreshError::InvalidEndpoint.persisted_summary(),
            "source endpoint is invalid"
        );
        assert_eq!(
            RefreshError::UnsupportedSource.persisted_summary(),
            "source type is not supported by the refresh worker"
        );
        assert_eq!(
            RefreshError::Catalog.persisted_summary(),
            "source catalog reconciliation failed"
        );
    }

    #[test]
    fn refresh_progress_maps_each_ingest_phase_to_the_public_stage_contract() {
        // 123 bytes maps to percent=1 during downloading (log scale, clamped).
        for (phase, stage, percent, message) in [
            ("downloading", "downloading", 1, "Downloading source data"),
            ("downloaded", "downloading", 15, "Downloaded source data"),
            ("decoded", "parsing", 30, "Decoded source artifact"),
            ("parsed", "parsing", 45, "Parsed source records"),
            ("staging", "parsing", 55, "Staged 0 of 7 source records"),
            ("activated", "activating", 70, "Activated snapshot"),
            (
                "reconciling-channels",
                "reconciling",
                85,
                "Reconciling channels",
            ),
            (
                "reconciling-epg",
                "reconciling",
                97,
                "Reconciled 0 channels",
            ),
            (
                "reconciling-events",
                "reconciling",
                98,
                "Reconciled 0 guide mappings",
            ),
            (
                "reconciling-complete",
                "reconciling",
                99,
                "Updated 0 dynamic event channels",
            ),
            ("unknown", "downloading", 0, "Refreshing source"),
        ] {
            let progress = refresh_progress_json(&IngestProgress {
                phase: phase.to_owned(),
                downloaded_bytes: 123,
                decoded_bytes: 0,
                records_seen: 7,
                records_prepared: 0,
            });
            assert_eq!(progress["stage"], stage);
            assert_eq!(progress["percent"], percent);
            assert_eq!(progress["bytesDownloaded"], 123);
            let expected_records = if phase.starts_with("reconciling") || phase == "staging" {
                0
            } else {
                7
            };
            assert_eq!(progress["recordsProcessed"], expected_records);
            assert_eq!(progress["message"], message);
        }
    }

    #[test]
    fn reconciliation_batch_progress_is_monotonic_after_activation() {
        let phases = [
            ("reconciling-snapshot", 0, 10),
            ("reconciling-channel-batch", 10, 10),
            ("reconciling-link-batch", 10, 10),
            ("reconciling-orphan-batch", 10, 10),
            ("reconciling-revision", 10, 10),
            ("reconciling-epg", 0, 10),
            ("reconciling-events", 0, 10),
            ("reconciling-complete", 0, 10),
        ];
        let percents: Vec<u64> = phases
            .into_iter()
            .map(|(phase, prepared, seen)| {
                refresh_progress_json(&IngestProgress {
                    phase: phase.to_owned(),
                    downloaded_bytes: 0,
                    decoded_bytes: 0,
                    records_seen: seen,
                    records_prepared: prepared,
                })["percent"]
                    .as_u64()
                    .expect("progress percent")
            })
            .collect();
        assert!(percents.windows(2).all(|window| window[0] <= window[1]));
    }

    #[test]
    fn refresh_progress_download_percent_scales_with_bytes() {
        // 0 bytes = 1%, 1 MB = 1%, 4 MB = 4%, 50 MB = ~9%, 100 MB = ~11%.
        let zero = refresh_progress_json(&IngestProgress {
            phase: "downloading".to_owned(),
            ..IngestProgress::default()
        });
        assert_eq!(zero["percent"], 1);
        let one_mb = refresh_progress_json(&IngestProgress {
            phase: "downloading".to_owned(),
            downloaded_bytes: 1_048_576,
            ..IngestProgress::default()
        });
        assert_eq!(one_mb["percent"], 1);
        let fifty_mb = refresh_progress_json(&IngestProgress {
            phase: "downloading".to_owned(),
            downloaded_bytes: 50 * 1_048_576,
            ..IngestProgress::default()
        });
        let pct = fifty_mb["percent"].as_u64().unwrap();
        assert!(
            (8..=14).contains(&pct),
            "50MB should map to 8-14%, got {pct}"
        );
        let huge = refresh_progress_json(&IngestProgress {
            phase: "downloading".to_owned(),
            downloaded_bytes: 500 * 1_048_576,
            ..IngestProgress::default()
        });
        assert_eq!(huge["percent"], 14);
    }

    #[test]
    fn completed_refresh_progress_reports_terminal_100_percent() {
        let result = ingest_result();
        for (source_refresh, message) in [
            (true, "Source refresh completed"),
            (false, "Xtream short EPG refresh completed"),
        ] {
            let progress = completed_refresh_progress(source_refresh, &result);
            assert_eq!(progress["stage"], "completed");
            assert_eq!(progress["percent"], 100);
            assert_eq!(progress["bytesDownloaded"], 12);
            assert_eq!(progress["recordsProcessed"], 100);
            assert_eq!(progress["message"], message);
        }
    }

    #[test]
    fn refresh_progress_advances_for_each_staging_batch() {
        let total = 1_000;
        for (staged, percent) in [(0, 55), (500, 62), (1_000, 70)] {
            let progress = refresh_progress_json(&IngestProgress {
                phase: "staging".to_owned(),
                downloaded_bytes: 1,
                decoded_bytes: 1,
                records_seen: total,
                records_prepared: staged,
            });
            assert_eq!(progress["percent"], percent);
            assert_eq!(progress["recordsProcessed"], staged);
            assert_eq!(
                progress["message"],
                format!("Staged {staged} of {total} source records")
            );
        }
    }

    #[test]
    fn staged_provider_progress_reports_partition_work() {
        let staged = refresh_progress_json(&IngestProgress {
            phase: "staged".to_owned(),
            records_seen: 1_000,
            ..IngestProgress::default()
        });
        assert_eq!(staged["stage"], "reconciling");
        assert_eq!(staged["percent"], 70);
        let partitions = refresh_progress_json(&IngestProgress {
            phase: "reconciling-partitions".to_owned(),
            records_seen: 1_000,
            ..IngestProgress::default()
        });
        assert_eq!(partitions["stage"], "reconciling");
        assert_eq!(partitions["percent"], 85);
    }

    #[test]
    fn reconciliation_partition_payload_requires_valid_nonnegative_values() {
        let run_id = Uuid::now_v7();
        let source_id = Uuid::now_v7();
        let parent_job_id = Uuid::now_v7();
        let payload = serde_json::json!({
            "runId": run_id.to_string(),
            "partitionNumber": "2",
            "sourceId": source_id.to_string(),
            "parentJobId": parent_job_id.to_string(),
        });
        assert_eq!(
            provider_reconciliation_partition_from_payload(&payload),
            Some((run_id, 2, source_id, parent_job_id))
        );
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"runId": "not-a-uuid", "partitionNumber": "2"}),
            serde_json::json!({"runId": run_id.to_string(), "partitionNumber": "-1", "sourceId": source_id.to_string(), "parentJobId": parent_job_id.to_string()}),
            serde_json::json!({"runId": run_id.to_string(), "partitionNumber": 2, "sourceId": source_id.to_string(), "parentJobId": parent_job_id.to_string()}),
        ] {
            assert!(provider_reconciliation_partition_from_payload(&payload).is_none());
        }
    }

    #[test]
    fn malformed_reconciliation_payload_can_still_fail_its_parent() {
        let parent_job_id = Uuid::now_v7();
        assert_eq!(
            reconciliation_parent_from_payload(&serde_json::json!({
                "parentJobId": parent_job_id.to_string(),
                "runId": "invalid",
            })),
            Some(parent_job_id)
        );
        assert_eq!(
            reconciliation_parent_from_payload(&serde_json::json!({
                "parentJobId": "invalid",
            })),
            None
        );
    }

    #[test]
    fn reconciliation_partition_progress_stays_in_reconciliation_range() {
        let run_id = Uuid::now_v7();
        let progress =
            |completed_keys, total_keys| iptv_persistence::ProviderReconciliationRunProgress {
                run_id,
                status: "processing".to_owned(),
                partition_count: 3,
                partitions_completed: 0,
                total_keys,
                completed_keys,
            };
        assert_eq!(reconciliation_partition_percent(&progress(0, 10)), 85);
        assert_eq!(reconciliation_partition_percent(&progress(5, 10)), 92);
        assert_eq!(reconciliation_partition_percent(&progress(10, 10)), 99);
        assert_eq!(reconciliation_partition_percent(&progress(100, 10)), 99);
        assert_eq!(reconciliation_partition_percent(&progress(0, 0)), 85);
    }

    #[tokio::test]
    async fn database_reports_missing_database_url_before_connecting() {
        // The process test environment does not set this variable.
        if env::var_os("DATABASE_URL").is_some() {
            return;
        }
        let error = database().await.expect_err("missing database URL");
        assert!(error.to_string().contains("DATABASE_URL"));
    }

    async fn integration_database() -> Option<Database> {
        let url = std::env::var("IPTV_TEST_DATABASE_URL").ok()?;
        let database = Database::connect(&url, 4)
            .await
            .expect("connect to test database");
        database.migrate().await.expect("run migrations");
        Some(database)
    }

    async fn isolated_integration_database() -> Option<(Database, Database, String)> {
        let database_url = std::env::var("IPTV_TEST_DATABASE_URL").ok()?;
        let admin = Database::connect(&database_url, 2)
            .await
            .expect("connect to test database");
        let schema = format!("iptv_gateway_test_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(admin.pool())
            .await
            .expect("create isolated test schema");

        let mut url = Url::parse(&database_url).expect("parse test database URL");
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema},public"));
        let database = Database::connect(url.as_str(), 4)
            .await
            .expect("connect to isolated test schema");
        database.migrate().await.expect("run isolated migrations");
        Some((admin, database, schema))
    }

    async fn drop_isolated_schema(admin: &Database, schema: &str) {
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(admin.pool())
            .await
            .expect("drop isolated test schema");
    }

    async fn force_running(pool: &sqlx::PgPool, job_id: Uuid, worker_id: &str) {
        sqlx::query(
            "UPDATE jobs SET status = 'running', locked_by = $2, locked_at = now(), \
             attempts = 1, heartbeat_at = now() WHERE id = $1",
        )
        .bind(job_id)
        .bind(worker_id)
        .execute(pool)
        .await
        .expect("force job to running");
    }

    async fn fetch_job(pool: &sqlx::PgPool, job_id: Uuid) -> JobRecord {
        sqlx::query_as::<_, JobRecord>("SELECT * FROM jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("fetch job record")
    }

    async fn job_status(pool: &sqlx::PgPool, job_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("fetch job status")
    }

    async fn job_last_error(pool: &sqlx::PgPool, job_id: Uuid) -> Option<String> {
        sqlx::query_scalar("SELECT last_error FROM jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("fetch job error")
    }

    async fn delete_job(pool: &sqlx::PgPool, job_id: Uuid) {
        sqlx::query("DELETE FROM jobs WHERE id = $1")
            .bind(job_id)
            .execute(pool)
            .await
            .expect("delete job");
    }

    async fn delete_source(pool: &sqlx::PgPool, source_id: Uuid) {
        let mut transaction = pool.begin().await.expect("begin cleanup transaction");
        sqlx::query("DELETE FROM jobs WHERE payload->>'sourceId' = $1")
            .bind(source_id.to_string())
            .execute(&mut *transaction)
            .await
            .expect("delete source jobs");
        sqlx::query("DELETE FROM audit_events WHERE resource_id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .expect("delete audit events");
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider account");
        sqlx::query("DELETE FROM epg_sources WHERE id = $1")
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .expect("delete epg source");
        transaction.commit().await.expect("commit cleanup");
    }

    #[tokio::test]
    async fn process_job_completes_noop_jobs() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-noop";
        let job = jobs
            .enqueue(&iptv_persistence::NewJob::immediate(
                "noop",
                serde_json::json!({}),
            ))
            .await
            .expect("enqueue noop");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process noop job");
        assert_eq!(job_status(&pool, job.id).await, "succeeded");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn process_job_fails_unsupported_job_kinds() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-unsupported";
        let mut new_job =
            iptv_persistence::NewJob::immediate("unsupported-kind", serde_json::json!({}));
        new_job.max_attempts = 1;
        let job = jobs.enqueue(&new_job).await.expect("enqueue unsupported");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process unsupported job");
        assert_eq!(job_status(&pool, job.id).await, "failed");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn process_job_fails_refresh_source_with_invalid_payload() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-invalid-payload";
        let mut new_job = iptv_persistence::NewJob::immediate(
            "refresh-source",
            serde_json::json!({"notSourceId": true}),
        );
        new_job.max_attempts = 1;
        let job = jobs
            .enqueue(&new_job)
            .await
            .expect("enqueue refresh with bad payload");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process refresh with bad payload");
        assert_eq!(job_status(&pool, job.id).await, "failed");
        let error = job_last_error(&pool, job.id)
            .await
            .expect("error was persisted");
        assert!(error.contains("refresh job payload is invalid"));
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn recover_stale_jobs_requeues_expired_jobs_with_attempts_left() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());

        let mut new_job = iptv_persistence::NewJob::immediate("noop", serde_json::json!({}));
        new_job.max_attempts = 3;
        let job = jobs.enqueue(&new_job).await.expect("enqueue job");
        // Simulate a worker that claimed the job, then stopped its heartbeat.
        sqlx::query(
            "UPDATE jobs SET status = 'running', locked_by = $2, locked_at = now(), \
             attempts = 1, heartbeat_at = now() - interval '6 minutes' WHERE id = $1",
        )
        .bind(job.id)
        .bind("dead-worker")
        .execute(&pool)
        .await
        .expect("force job to running");

        let recovered = jobs
            .recover_stale_jobs(300)
            .await
            .expect("recover stale jobs");
        assert!(recovered >= 1);
        assert_eq!(job_status(&pool, job.id).await, "queued");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn recover_stale_jobs_fails_expired_jobs_at_max_attempts() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());

        let mut new_job = iptv_persistence::NewJob::immediate("noop", serde_json::json!({}));
        new_job.max_attempts = 1;
        let job = jobs.enqueue(&new_job).await.expect("enqueue job");
        // Simulate a worker that exhausted all attempts and lost its lease.
        sqlx::query(
            "UPDATE jobs SET status = 'running', locked_by = $2, locked_at = now(), \
             attempts = 1, max_attempts = 1, \
             heartbeat_at = now() - interval '6 minutes' WHERE id = $1",
        )
        .bind(job.id)
        .bind("dead-worker")
        .execute(&pool)
        .await
        .expect("force job to running at max attempts");

        let recovered = jobs
            .recover_stale_jobs(300)
            .await
            .expect("recover stale jobs");
        assert!(recovered >= 1);
        assert_eq!(job_status(&pool, job.id).await, "failed");
        let error = job_last_error(&pool, job.id)
            .await
            .expect("error was persisted");
        assert_eq!(error, "worker lease expired");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn reset_stranded_checking_streams_returns_checking_to_unknown() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let catalog = CatalogRepository::new(pool.clone());
        let suffix = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let snapshot_id = Uuid::now_v7();
        let stream_id = Uuid::now_v7();

        let mut transaction = pool.begin().await.expect("begin transaction");
        sqlx::query(
            "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) \
             VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
        )
        .bind(account_id)
        .bind(format!("Stranded reset test {suffix}"))
        .execute(&mut *transaction)
        .await
        .expect("insert provider account");
        sqlx::query(
            "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
             VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .expect("insert snapshot");
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, supported) \
             VALUES ($1, $2, $3, $4, $5, 'news', 'https://provider.test/stream', true)",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("stranded-reset-{suffix}"))
        .bind("Stranded Reset Stream")
        .execute(&mut *transaction)
        .await
        .expect("insert provider stream");
        transaction.commit().await.expect("commit");

        sqlx::query("UPDATE provider_streams SET health_status = 'checking' WHERE id = $1")
            .bind(stream_id)
            .execute(&pool)
            .await
            .expect("mark stream as checking");

        let reset = catalog
            .reset_stranded_checking_streams()
            .await
            .expect("reset stranded checking streams");
        assert!(reset >= 1);

        let status: String =
            sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
                .bind(stream_id)
                .fetch_one(&pool)
                .await
                .expect("fetch stream status");
        assert_eq!(status, "unknown");

        let mut transaction = pool.begin().await.expect("begin cleanup");
        sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete health checks");
        sqlx::query("DELETE FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider stream");
        sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .expect("delete snapshot");
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider account");
        transaction.commit().await.expect("commit cleanup");
    }

    #[tokio::test]
    async fn process_job_fails_refresh_source_when_source_is_missing() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-missing-source";
        let source_id = Uuid::now_v7();
        let mut new_job = iptv_persistence::NewJob::immediate(
            "refresh-source",
            serde_json::json!({"sourceId": source_id}),
        );
        new_job.max_attempts = 1;
        let job = jobs
            .enqueue(&new_job)
            .await
            .expect("enqueue refresh for missing source");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process refresh for missing source");
        assert_eq!(job_status(&pool, job.id).await, "failed");
        let error = job_last_error(&pool, job.id)
            .await
            .expect("error was persisted");
        assert!(error.contains("source configuration could not be loaded"));
        delete_job(&pool, job.id).await;
    }

    #[test]
    fn decrypt_probe_url_returns_template_without_ciphertext() {
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let target = StreamProbeTargetRow {
            provider_stream_id: Uuid::now_v7(),
            provider_account_id: Uuid::now_v7(),
            provider_pool_id: Uuid::now_v7(),
            max_connections: 2,
            input_adapter: "auto".to_owned(),
            url_template: "https://provider.test/stream".to_owned(),
            url_secret_ciphertext: None,
        };
        let url = decrypt_probe_url(&master_key, &target).expect("template fallback succeeds");
        assert_eq!(url, "https://provider.test/stream");
    }

    #[test]
    fn decrypt_probe_url_decrypts_ciphertext_when_present() {
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let account_id = Uuid::now_v7();
        let plaintext = b"https://secret.provider.test/stream";
        let associated_data = format!("iptv-provider-stream:v1:{account_id}");
        let ciphertext = master_key
            .encrypt_secret(plaintext, associated_data.as_bytes())
            .expect("encrypt URL");
        let target = StreamProbeTargetRow {
            provider_stream_id: Uuid::now_v7(),
            provider_account_id: account_id,
            provider_pool_id: Uuid::now_v7(),
            max_connections: 2,
            input_adapter: "auto".to_owned(),
            url_template: "https://provider.test/[encrypted]".to_owned(),
            url_secret_ciphertext: Some(ciphertext),
        };
        let url = decrypt_probe_url(&master_key, &target).expect("decrypt URL");
        assert_eq!(url, "https://secret.provider.test/stream");
    }

    #[test]
    fn decrypt_probe_url_rejects_invalid_ciphertext() {
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let target = StreamProbeTargetRow {
            provider_stream_id: Uuid::now_v7(),
            provider_account_id: Uuid::now_v7(),
            provider_pool_id: Uuid::now_v7(),
            max_connections: 2,
            input_adapter: "auto".to_owned(),
            url_template: "https://provider.test/[encrypted]".to_owned(),
            url_secret_ciphertext: Some(vec![0_u8, 1, 2, 3]),
        };
        assert!(decrypt_probe_url(&master_key, &target).is_err());
    }

    #[tokio::test]
    async fn process_job_fails_health_probe_with_invalid_payload() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-health-probe-invalid";
        let mut new_job = iptv_persistence::NewJob::immediate(
            "health-probe",
            serde_json::json!({"notProviderStreamId": true}),
        );
        new_job.max_attempts = 1;
        let job = jobs
            .enqueue(&new_job)
            .await
            .expect("enqueue health-probe with bad payload");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process health-probe with bad payload");
        assert_eq!(job_status(&pool, job.id).await, "failed");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn process_job_succeeds_health_probe_for_missing_stream() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-health-probe-missing";
        let stream_id = Uuid::now_v7();
        let mut new_job = iptv_persistence::NewJob::immediate(
            "health-probe",
            serde_json::json!({"providerStreamId": stream_id}),
        );
        new_job.max_attempts = 1;
        let job = jobs
            .enqueue(&new_job)
            .await
            .expect("enqueue health-probe for missing stream");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process health-probe for missing stream");
        assert_eq!(job_status(&pool, job.id).await, "succeeded");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn run_health_probe_returns_skipped_for_already_checking_stream() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let suffix = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let snapshot_id = Uuid::now_v7();
        let stream_id = Uuid::now_v7();

        let mut transaction = pool.begin().await.expect("begin transaction");
        sqlx::query(
            "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) \
             VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
        )
        .bind(account_id)
        .bind(format!("Health probe test {suffix}"))
        .execute(&mut *transaction)
        .await
        .expect("insert provider account");
        sqlx::query(
            "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
             VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .expect("insert snapshot");
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, supported) \
             VALUES ($1, $2, $3, $4, $5, 'news', 'https://provider.test/stream', true)",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("probe-skip-{suffix}"))
        .bind("Probe Skip Stream")
        .execute(&mut *transaction)
        .await
        .expect("insert provider stream");
        transaction.commit().await.expect("commit");

        // Mark the stream as `checking` so the probe must skip it.
        sqlx::query("UPDATE provider_streams SET health_status = 'checking' WHERE id = $1")
            .bind(stream_id)
            .execute(&pool)
            .await
            .expect("mark stream as checking");

        let result = run_health_probe(&catalog, &master_key, stream_id)
            .await
            .expect("probe call succeeds");
        assert_eq!(result, HealthProbeResult::Skipped);

        // Cleanup.
        let mut transaction = pool.begin().await.expect("begin cleanup");
        sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete health checks");
        sqlx::query("DELETE FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider stream");
        sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .expect("delete snapshot");
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider account");
        transaction.commit().await.expect("commit cleanup");
    }

    #[tokio::test]
    async fn process_job_succeeds_health_probe_for_already_checking_stream() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-health-probe-checking";
        let suffix = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let snapshot_id = Uuid::now_v7();
        let stream_id = Uuid::now_v7();

        let mut transaction = pool.begin().await.expect("begin transaction");
        sqlx::query(
            "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) \
             VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
        )
        .bind(account_id)
        .bind(format!("Checking probe test {suffix}"))
        .execute(&mut *transaction)
        .await
        .expect("insert provider account");
        sqlx::query(
            "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
             VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .expect("insert snapshot");
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, supported) \
             VALUES ($1, $2, $3, $4, $5, 'news', 'https://provider.test/stream', true)",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("checking-{suffix}"))
        .bind("Checking Probe Stream")
        .execute(&mut *transaction)
        .await
        .expect("insert provider stream");
        transaction.commit().await.expect("commit");

        // Mark the stream as `checking` so the probe must skip it.
        sqlx::query("UPDATE provider_streams SET health_status = 'checking' WHERE id = $1")
            .bind(stream_id)
            .execute(&pool)
            .await
            .expect("mark stream as checking");

        let mut new_job = iptv_persistence::NewJob::immediate(
            "health-probe",
            serde_json::json!({"providerStreamId": stream_id}),
        );
        new_job.max_attempts = 1;
        let job = jobs
            .enqueue(&new_job)
            .await
            .expect("enqueue health-probe for checking stream");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process health-probe for checking stream");
        assert_eq!(job_status(&pool, job.id).await, "succeeded");

        // Cleanup.
        delete_job(&pool, job.id).await;
        let mut transaction = pool.begin().await.expect("begin cleanup");
        sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete health checks");
        sqlx::query("DELETE FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider stream");
        sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .expect("delete snapshot");
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider account");
        transaction.commit().await.expect("commit cleanup");
    }

    const PROBE_PAT_PID: u16 = 0x0000;
    const PROBE_PMT_PID: u16 = 0x100;
    const PROBE_VIDEO_PID: u16 = 0x101;
    const PROBE_AUDIO_PID: u16 = 0x102;
    const PROBE_PKT_SIZE: usize = iptv_media::MPEG_TS_PACKET_SIZE;

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn run_health_probe_persists_alive_status_for_a_live_stream() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let suffix = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let snapshot_id = Uuid::now_v7();
        let stream_id = Uuid::now_v7();

        // Build a valid TS chunk with PAT, PMT, and payload packets. The
        // packet structure mirrors the media crate test helpers but is
        // self-contained because `psi::test_support` is `pub(crate)`.
        // PAT section: table_id=0x00, section_length=0x0d, tsid=1,
        // program_number=1, PMT PID=0x100.
        let mut association_section: Vec<u8> = vec![
            0x00,
            0xb0,
            0x0d,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0x00,
            0x01,
            0xe0 | 0x01,
            0x00,
        ];
        // PMT section: table_id=0x02, program_number=1, PCR_PID=VIDEO_PID,
        // one H.264 video stream and one MP2 audio stream.
        let mut program_map_section: Vec<u8> = vec![
            0x02,
            0xb0,
            0x17,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0xe0 | 0x01,
            0x01,
            0xf0,
            0x00,
            0x1b,
            0xe0 | 0x01,
            0x01,
            0xf0,
            0x00, // Video: H.264 (stream_type 0x1b).
            0x04,
            0xe0 | 0x01,
            0x02,
            0xf0,
            0x00, // Audio: MP2 (stream_type 0x04).
        ];

        // Compute and append MPEG CRC32 for each section.
        let association_crc = mpeg_crc32(&association_section);
        association_section.extend_from_slice(&association_crc.to_be_bytes());
        let program_map_crc = mpeg_crc32(&program_map_section);
        program_map_section.extend_from_slice(&program_map_crc.to_be_bytes());

        let mut ts_bytes = Vec::new();

        // PAT packet.
        let mut association_packet = [0xff_u8; PROBE_PKT_SIZE];
        association_packet[0] = 0x47;
        association_packet[1] = 0x40 | u8::try_from((PROBE_PAT_PID >> 8) & 0x1f).unwrap();
        association_packet[2] = u8::try_from(PROBE_PAT_PID & 0xff).unwrap();
        association_packet[3] = 0x10;
        association_packet[4] = 0x00; // pointer field
        association_packet[5..5 + association_section.len()].copy_from_slice(&association_section);
        ts_bytes.extend_from_slice(&association_packet);

        // PMT packet.
        let mut program_map_packet = [0xff_u8; PROBE_PKT_SIZE];
        program_map_packet[0] = 0x47;
        program_map_packet[1] = 0x40 | u8::try_from((PROBE_PMT_PID >> 8) & 0x1f).unwrap();
        program_map_packet[2] = u8::try_from(PROBE_PMT_PID & 0xff).unwrap();
        program_map_packet[3] = 0x10;
        program_map_packet[4] = 0x00; // pointer field
        program_map_packet[5..5 + program_map_section.len()].copy_from_slice(&program_map_section);
        ts_bytes.extend_from_slice(&program_map_packet);

        // Payload packets on video and audio PIDs.
        for index in 0u8..40 {
            let pid = if index.is_multiple_of(2) {
                PROBE_VIDEO_PID
            } else {
                PROBE_AUDIO_PID
            };
            let mut packet = [0xff_u8; PROBE_PKT_SIZE];
            packet[0] = 0x47;
            packet[1] = u8::try_from((pid >> 8) & 0x1f).unwrap();
            packet[2] = u8::try_from(pid & 0xff).unwrap();
            packet[3] = 0x10 | (index & 0x0f);
            ts_bytes.extend_from_slice(&packet);
        }

        // Start a local HTTP server that serves the TS data.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local address");
        let ts_data = ts_bytes.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = [0_u8; 1024];
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    tokio::io::AsyncReadExt::read(&mut socket, &mut buf),
                )
                .await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: {}\r\n\r\n",
                    ts_data.len()
                );
                tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                    .await
                    .expect("write headers");
                tokio::io::AsyncWriteExt::write_all(&mut socket, &ts_data)
                    .await
                    .expect("write body");
            }
        });

        let url = format!("http://{addr}/stream.ts");

        let mut transaction = pool.begin().await.expect("begin transaction");
        sqlx::query(
            "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) \
             VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
        )
        .bind(account_id)
        .bind(format!("Live probe test {suffix}"))
        .execute(&mut *transaction)
        .await
        .expect("insert provider account");
        sqlx::query(
            "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
             VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .expect("insert snapshot");
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, supported) \
             VALUES ($1, $2, $3, $4, $5, 'news', $6, true)",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("live-probe-{suffix}"))
        .bind("Live Probe Stream")
        .bind(&url)
        .execute(&mut *transaction)
        .await
        .expect("insert provider stream");
        transaction.commit().await.expect("commit");

        let core_app = Router::new()
            .route(
                "/internal/v1/provider-reservations",
                post(|| async { Json(serde_json::json!({"reservation_id": "probe-1"})) }),
            )
            .route(
                "/internal/v1/provider-reservations/{reservation_id}",
                delete(|| async { StatusCode::NO_CONTENT }),
            );
        let core_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind core test server");
        let core_address = core_listener.local_addr().expect("core local address");
        let core_server = tokio::spawn(async move {
            axum::serve(core_listener, core_app).await.unwrap();
        });

        let result_without_core =
            run_health_probe_with_core_config(&catalog, &master_key, stream_id, None)
                .await
                .expect("probe call succeeds without core reservation");
        assert_eq!(result_without_core, HealthProbeResult::Applied);
        sqlx::query("UPDATE provider_streams SET health_status = 'unknown' WHERE id = $1")
            .bind(stream_id)
            .execute(&pool)
            .await
            .expect("reset stream health for core reservation probe");

        let result = run_health_probe_with_core_config(
            &catalog,
            &master_key,
            stream_id,
            Some((
                format!("http://{core_address}"),
                "test-bootstrap-token".to_owned(),
            )),
        )
        .await
        .expect("probe call succeeds");
        assert_eq!(result, HealthProbeResult::Applied);
        core_server.abort();

        let status: String =
            sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
                .bind(stream_id)
                .fetch_one(&pool)
                .await
                .expect("fetch alive status");
        assert_eq!(status, "alive");

        // A full core pool must release the local checking claim and skip the
        // probe without changing the stream health state.
        sqlx::query("UPDATE provider_streams SET health_status = 'unknown' WHERE id = $1")
            .bind(stream_id)
            .execute(&pool)
            .await
            .expect("reset stream health for conflict probe");
        let conflict_app = Router::new().route(
            "/internal/v1/provider-reservations",
            post(|| async { StatusCode::CONFLICT }),
        );
        let conflict_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind conflict core server");
        let conflict_address = conflict_listener
            .local_addr()
            .expect("conflict core local address");
        let conflict_server = tokio::spawn(async move {
            axum::serve(conflict_listener, conflict_app).await.unwrap();
        });
        let skipped = run_health_probe_with_core_config(
            &catalog,
            &master_key,
            stream_id,
            Some((
                format!("http://{conflict_address}"),
                "test-bootstrap-token".to_owned(),
            )),
        )
        .await
        .expect("capacity conflict is a normal skip");
        assert_eq!(skipped, HealthProbeResult::Skipped);
        conflict_server.abort();

        // The skipped probe must release its checking claim.
        let status: String =
            sqlx::query_scalar("SELECT health_status FROM provider_streams WHERE id = $1")
                .bind(stream_id)
                .fetch_one(&pool)
                .await
                .expect("fetch status");
        assert_eq!(status, "unknown");

        let _ = server.await;

        // Cleanup.
        let mut transaction = pool.begin().await.expect("begin cleanup");
        sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete health checks");
        sqlx::query("DELETE FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider stream");
        sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .expect("delete snapshot");
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider account");
        transaction.commit().await.expect("commit cleanup");
    }

    #[tokio::test]
    async fn health_probe_scheduler_starts_and_stops_on_timeout() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let catalog = CatalogRepository::new(pool.clone());
        let jobs = JobRepository::new(pool.clone());

        // The scheduler loops forever. A short timeout covers the startup
        // lines and the first `tokio::select!` before the scheduler interval
        // sleep completes.
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            health_probe_scheduler(&catalog, &jobs),
        )
        .await;
        assert!(result.is_err(), "scheduler must not return on its own");
    }

    #[tokio::test]
    async fn process_job_fails_health_probe_when_decryption_fails() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-health-probe-decrypt-fail";
        let suffix = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let snapshot_id = Uuid::now_v7();
        let stream_id = Uuid::now_v7();

        let mut transaction = pool.begin().await.expect("begin transaction");
        sqlx::query(
            "INSERT INTO provider_accounts (id, name, source_type, base_url_template, max_connections) \
             VALUES ($1, $2, 'm3u', 'https://provider.test/', 2)",
        )
        .bind(account_id)
        .bind(format!("Decrypt fail test {suffix}"))
        .execute(&mut *transaction)
        .await
        .expect("insert provider account");
        sqlx::query(
            "INSERT INTO source_snapshots (id, provider_account_id, kind, status, checksum_sha256, byte_count, record_count) \
             VALUES ($1, $2, 'm3u', 'active', $3, 1, 1)",
        )
        .bind(snapshot_id)
        .bind(account_id)
        .bind(snapshot_id.to_string())
        .execute(&mut *transaction)
        .await
        .expect("insert snapshot");
        // Insert a stream with invalid ciphertext so `decrypt_probe_url` fails.
        sqlx::query(
            "INSERT INTO provider_streams (id, snapshot_id, provider_account_id, stable_key, name, group_name, url_template, url_secret_ciphertext, supported) \
             VALUES ($1, $2, $3, $4, $5, 'news', 'https://provider.test/[encrypted]', $6, true)",
        )
        .bind(stream_id)
        .bind(snapshot_id)
        .bind(account_id)
        .bind(format!("decrypt-fail-{suffix}"))
        .bind("Decrypt Fail Stream")
        .bind(vec![0_u8, 1, 2, 3])
        .execute(&mut *transaction)
        .await
        .expect("insert provider stream");
        transaction.commit().await.expect("commit");

        let mut new_job = iptv_persistence::NewJob::immediate(
            "health-probe",
            serde_json::json!({"providerStreamId": stream_id}),
        );
        new_job.max_attempts = 1;
        let job = jobs
            .enqueue(&new_job)
            .await
            .expect("enqueue health-probe for decrypt fail");
        force_running(&pool, job.id, worker_id).await;
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process health-probe for decrypt fail");
        assert_eq!(job_status(&pool, job.id).await, "failed");

        // Cleanup.
        delete_job(&pool, job.id).await;
        let mut transaction = pool.begin().await.expect("begin cleanup");
        sqlx::query("DELETE FROM stream_health_checks WHERE provider_stream_id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete health checks");
        sqlx::query("DELETE FROM provider_streams WHERE id = $1")
            .bind(stream_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider stream");
        sqlx::query("DELETE FROM source_snapshots WHERE id = $1")
            .bind(snapshot_id)
            .execute(&mut *transaction)
            .await
            .expect("delete snapshot");
        sqlx::query("DELETE FROM provider_accounts WHERE id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .expect("delete provider account");
        transaction.commit().await.expect("commit cleanup");
    }

    #[tokio::test]
    async fn process_job_leaves_a_cancelled_refresh_in_cancelled_state() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), MasterKey::from_bytes([1_u8; 32]));
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let worker_id = "test-worker-cancelled-refresh";
        let mut new_job = iptv_persistence::NewJob::immediate(
            "refresh-source",
            serde_json::json!({"notSourceId": true}),
        );
        new_job.max_attempts = 1;
        let job = jobs.enqueue(&new_job).await.expect("enqueue refresh job");
        force_running(&pool, job.id, worker_id).await;
        jobs.cancel(job.id).await.expect("cancel refresh job");
        let record = fetch_job(&pool, job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process cancelled refresh");
        assert_eq!(job_status(&pool, job.id).await, "cancelled");
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn process_job_refreshes_xtream_and_preserves_active_catalog_on_failure() {
        let Some((admin, database, schema)) = isolated_integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind Xtream fixture");
        let address = listener.local_addr().expect("Xtream fixture address");
        let fixture = Arc::new(XtreamFixtureState::default());
        fixture.set_phase(XTREAM_PHASE_INITIAL);
        let fixture_server = tokio::spawn({
            let fixture = Arc::clone(&fixture);
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/player_api.php", get(xtream_fixture_response))
                        .with_state(fixture),
                )
                .await
                .expect("serve Xtream fixture");
            }
        });

        let pool = database.pool().clone();
        let master_key = MasterKey::from_bytes([8_u8; 32]);
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), master_key.clone());
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let worker_id = "test-worker-xtream-success";
        let source = sources
            .create(
                &iptv_persistence::NewSource {
                    name: format!("Xtream worker test {}", Uuid::now_v7()),
                    kind: SourceKind::Xtream,
                    endpoint: format!(
                        "http://{address}/player_api.php?username=worker-user-canary&password=worker-password-canary&access_token=source-access-token-canary"
                    ),
                },
                "coverage-test",
            )
            .await
            .expect("create Xtream source");
        let source_id = source.source.id;
        process_refresh_job(
            &pool,
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            source.refresh_job.id,
        )
        .await;
        assert_eq!(job_status(&pool, source.refresh_job.id).await, "succeeded");
        assert_eq!(fixture.request_counts(), (1, 1, 1));

        let active_snapshot_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM source_snapshots \
             WHERE provider_account_id = $1 AND kind = 'xtream' AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("fetch active Xtream snapshot");
        let active_snapshot_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM source_snapshots \
             WHERE provider_account_id = $1 AND kind = 'xtream' AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("count active Xtream snapshots");
        assert_eq!(active_snapshot_count, 1);

        let (stable_key, group_name, endpoint_template, endpoint_ciphertext, attributes): (
            String,
            Option<String>,
            String,
            Option<Vec<u8>>,
            serde_json::Value,
        ) = sqlx::query_as(
            "SELECT stable_key, group_name, url_template, url_secret_ciphertext, attributes \
             FROM provider_streams WHERE snapshot_id = $1",
        )
        .bind(active_snapshot_id)
        .fetch_one(&pool)
        .await
        .expect("fetch Xtream provider stream");
        assert_eq!(stable_key, "xtream:7");
        assert_eq!(group_name.as_deref(), Some("Sports"));
        assert!(!endpoint_template.contains("{stream_id}"));
        assert_eq!(
            attributes
                .get("access_token")
                .and_then(serde_json::Value::as_str),
            Some("[REDACTED]")
        );
        let endpoint_ciphertext = endpoint_ciphertext.expect("encrypt Xtream stream endpoint");
        let decrypted_endpoint = master_key
            .decrypt_secret(
                &endpoint_ciphertext,
                format!("iptv-provider-stream:v1:{source_id}").as_bytes(),
            )
            .expect("decrypt concrete Xtream endpoint");
        let decrypted_endpoint = String::from_utf8(decrypted_endpoint).expect("endpoint text");
        assert!(
            decrypted_endpoint.ends_with("/live/worker-user-canary/worker-password-canary/7.ts")
        );
        assert!(!decrypted_endpoint.contains("{stream_id}"));

        let channel_id: Uuid = sqlx::query_scalar(
            "SELECT c.id FROM channels c \
             JOIN channel_streams cs ON cs.channel_id = c.id \
             JOIN provider_streams ps ON ps.id = cs.provider_stream_id \
             WHERE ps.snapshot_id = $1 AND ps.stable_key = 'xtream:7'",
        )
        .bind(active_snapshot_id)
        .fetch_one(&pool)
        .await
        .expect("fetch Xtream canonical channel link");

        fixture.set_phase(XTREAM_PHASE_RENAMED);
        let renamed_job = jobs
            .enqueue(&iptv_persistence::NewJob::immediate(
                "refresh-source",
                serde_json::json!({"sourceId": source_id}),
            ))
            .await
            .expect("enqueue renamed Xtream refresh");
        process_refresh_job(
            &pool,
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            renamed_job.id,
        )
        .await;
        assert_eq!(job_status(&pool, renamed_job.id).await, "succeeded");
        let refreshed_snapshot_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM source_snapshots \
             WHERE provider_account_id = $1 AND kind = 'xtream' AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("fetch refreshed Xtream snapshot");
        assert_ne!(refreshed_snapshot_id, active_snapshot_id);
        let refreshed_channel_id: Uuid = sqlx::query_scalar(
            "SELECT c.id FROM channels c \
             JOIN channel_streams cs ON cs.channel_id = c.id \
             JOIN provider_streams ps ON ps.id = cs.provider_stream_id \
             WHERE ps.snapshot_id = $1 AND ps.stable_key = 'xtream:7'",
        )
        .bind(refreshed_snapshot_id)
        .fetch_one(&pool)
        .await
        .expect("fetch refreshed Xtream canonical channel link");
        assert_eq!(refreshed_channel_id, channel_id);

        let counts_before_auth_failure = fixture.request_counts();
        fixture.set_phase(XTREAM_PHASE_AUTH_FAILURE);
        let auth_failure_job = jobs
            .enqueue(&NewJob::immediate(
                "refresh-source",
                serde_json::json!({"sourceId": source_id}),
            ))
            .await
            .expect("enqueue Xtream authentication failure");
        process_refresh_job(
            &pool,
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            auth_failure_job.id,
        )
        .await;
        assert_eq!(job_status(&pool, auth_failure_job.id).await, "failed");
        assert_eq!(
            fixture.request_counts(),
            (
                counts_before_auth_failure.0 + 1,
                counts_before_auth_failure.1,
                counts_before_auth_failure.2,
            )
        );

        let counts_before_empty_streams = fixture.request_counts();
        fixture.set_phase(XTREAM_PHASE_EMPTY_STREAMS);
        let empty_streams_job = jobs
            .enqueue(&NewJob::immediate(
                "refresh-source",
                serde_json::json!({"sourceId": source_id}),
            ))
            .await
            .expect("enqueue Xtream empty stream refresh");
        process_refresh_job(
            &pool,
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            empty_streams_job.id,
        )
        .await;
        assert_eq!(job_status(&pool, empty_streams_job.id).await, "failed");
        assert_eq!(
            fixture.request_counts(),
            (
                counts_before_empty_streams.0 + 1,
                counts_before_empty_streams.1 + 1,
                counts_before_empty_streams.2 + 1,
            )
        );

        let final_snapshot_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM source_snapshots \
             WHERE provider_account_id = $1 AND kind = 'xtream' AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("fetch preserved Xtream snapshot");
        assert_eq!(final_snapshot_id, refreshed_snapshot_id);
        let final_channel_id: Uuid = sqlx::query_scalar(
            "SELECT c.id FROM channels c \
             JOIN channel_streams cs ON cs.channel_id = c.id \
             JOIN provider_streams ps ON ps.id = cs.provider_stream_id \
             WHERE ps.snapshot_id = $1 AND ps.stable_key = 'xtream:7'",
        )
        .bind(final_snapshot_id)
        .fetch_one(&pool)
        .await
        .expect("fetch preserved Xtream channel link");
        assert_eq!(final_channel_id, channel_id);

        let public_or_diagnostic_data: String = sqlx::query_scalar(
            "SELECT coalesce(string_agg(value, E'\\n'), '') FROM ( \
                 SELECT base_url_template AS value FROM provider_accounts WHERE id = $1 \
                 UNION ALL \
                 SELECT diagnostics::text AS value FROM source_snapshots WHERE provider_account_id = $1 \
                 UNION ALL \
                 SELECT url_template AS value FROM provider_streams WHERE provider_account_id = $1 \
                 UNION ALL \
                 SELECT attributes::text AS value FROM provider_streams WHERE provider_account_id = $1 \
                 UNION ALL \
                 SELECT directives::text AS value FROM provider_streams WHERE provider_account_id = $1 \
                 UNION ALL \
                 SELECT payload::text AS value FROM jobs WHERE payload->>'sourceId' = $1::text \
                 UNION ALL \
                 SELECT progress::text AS value FROM jobs WHERE payload->>'sourceId' = $1::text \
                 UNION ALL \
                 SELECT coalesce(last_error, '') AS value FROM jobs WHERE payload->>'sourceId' = $1::text \
                 UNION ALL \
                 SELECT details::text AS value FROM audit_events WHERE resource_id = $1 \
             ) AS stored_values",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("read public and diagnostic data");
        for canary in [
            "worker-user-canary",
            "worker-password-canary",
            "source-access-token-canary",
            "stream-access-token-canary",
        ] {
            assert!(
                !public_or_diagnostic_data.contains(canary),
                "public or diagnostic data contained a credential canary"
            );
        }

        delete_source(&pool, source_id).await;
        fixture_server.abort();
        drop(database);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn process_job_completes_a_local_m3u_refresh_and_reconciles_catalog() {
        let Some((admin, database, schema)) = isolated_integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let suffix = Uuid::now_v7();
        let event_group = format!("Worker events {suffix}");
        let playlist = format!(
            "#EXTM3U\n#EXTINF:-1 tvg-id=\"coverage-local-{suffix}\" group-title=\"{event_group}\",Event 7: Broncos vs Chiefs 2026-09-14 00:20 HD\nhttp://provider.invalid/live.ts\n"
        );
        let parsed = iptv_parsers::parse_m3u(
            std::io::Cursor::new(&playlist),
            iptv_parsers::ParseLimits::default(),
        )
        .expect("parse isolated M3U fixture");
        assert_eq!(parsed.entries.len(), 1, "{:#?}", parsed.diagnostics);
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind source fixture");
        let address = listener.local_addr().expect("source fixture address");
        let source_app = Router::new().route(
            "/playlist.m3u",
            get(move || {
                let playlist = playlist.clone();
                async move { playlist }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(listener, source_app)
                .await
                .expect("serve source fixture");
        });

        let pool = database.pool().clone();
        let master_key = MasterKey::from_bytes([3_u8; 32]);
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), master_key.clone());
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let worker_id = "test-worker-m3u-success";
        let event_template = catalog
            .create_event_template(iptv_persistence::CreateEventTemplate {
                name: format!("worker-events-{suffix}"),
                display_name: "Worker sports".to_owned(),
                match_regex: "Broncos".to_owned(),
                channel_name_format: "{home} vs {away}".to_owned(),
                group_name: event_group,
                event_duration_hours: 3,
                past_date_grace_hours: 1,
                future_date_days: 1,
                timezone: "UTC".to_owned(),
                filler_title: "No programs available".to_owned(),
            })
            .await
            .expect("create event template");
        let created = sources
            .create(
                &iptv_persistence::NewSource {
                    name: format!("M3U coverage test {suffix}"),
                    kind: SourceKind::M3u,
                    endpoint: format!("http://{address}/playlist.m3u"),
                },
                "coverage-test",
            )
            .await
            .expect("create M3U source");
        let source_id = created.source.id;
        sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
            .bind(created.refresh_job.id)
            .execute(&pool)
            .await
            .expect("set max attempts");
        force_running(&pool, created.refresh_job.id, worker_id).await;
        let record = fetch_job(&pool, created.refresh_job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process M3U refresh");
        let status = job_status(&pool, created.refresh_job.id).await;
        assert_eq!(
            status,
            "succeeded",
            "M3U refresh failed: {:?}",
            job_last_error(&pool, created.refresh_job.id).await
        );
        let snapshot_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM source_snapshots WHERE provider_account_id = $1 AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("count active snapshots");
        assert_eq!(snapshot_count, 1);
        let event_channels = catalog
            .list_event_channels(Some(event_template.id))
            .await
            .expect("list generated event channels");
        assert_eq!(event_channels.len(), 1);
        let channel_id = event_channels[0].channel_id.expect("event channel target");
        let programmes = catalog
            .list_programmes_for_channels(&[channel_id])
            .await
            .expect("list generated programmes");
        assert_eq!(programmes.len(), 3);
        assert!(
            programmes
                .iter()
                .any(|programme| programme.title == "Broncos vs Chiefs")
        );

        catalog
            .delete_event_template(event_template.id)
            .await
            .expect("delete event template");
        delete_source(&pool, source_id).await;
        source_server.abort();
        drop(database);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn process_job_stops_ingest_when_a_refresh_is_cancelled() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let master_key = MasterKey::from_bytes([4_u8; 32]);
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), master_key.clone());
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let worker_id = "test-worker-cancelled-ingest";
        let suffix = Uuid::now_v7();
        let created = sources
            .create(
                &iptv_persistence::NewSource {
                    name: format!("Cancelled M3U coverage test {suffix}"),
                    kind: SourceKind::M3u,
                    endpoint: "http://127.0.0.1:1/never.m3u".to_owned(),
                },
                "coverage-test",
            )
            .await
            .expect("create M3U source");
        force_running(&pool, created.refresh_job.id, worker_id).await;
        jobs.cancel(created.refresh_job.id)
            .await
            .expect("cancel refresh job");
        let record = fetch_job(&pool, created.refresh_job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process cancelled ingest");
        assert_eq!(job_status(&pool, created.refresh_job.id).await, "cancelled");
        delete_source(&pool, created.source.id).await;
    }

    #[tokio::test]
    async fn process_job_completes_a_local_xmltv_refresh_and_reconciles_epg() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind EPG fixture");
        let address = listener.local_addr().expect("EPG fixture address");
        let source_app = Router::new().route(
            "/guide.xml",
            get(|| async {
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><tv><channel id=\"coverage-epg\"><display-name>Coverage EPG</display-name></channel><programme start=\"20260820130000 +0000\" stop=\"20260820140000 +0000\" channel=\"coverage-epg\"><title>Coverage Programme</title></programme></tv>"
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(listener, source_app)
                .await
                .expect("serve EPG fixture");
        });

        let pool = database.pool().clone();
        let master_key = MasterKey::from_bytes([5_u8; 32]);
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), master_key.clone());
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let worker_id = "test-worker-xmltv-success";
        let suffix = Uuid::now_v7();
        let created = sources
            .create(
                &iptv_persistence::NewSource {
                    name: format!("XMLTV coverage test {suffix}"),
                    kind: SourceKind::Xmltv,
                    endpoint: format!("http://{address}/guide.xml"),
                },
                "coverage-test",
            )
            .await
            .expect("create XMLTV source");
        let source_id = created.source.id;
        force_running(&pool, created.refresh_job.id, worker_id).await;
        let record = fetch_job(&pool, created.refresh_job.id).await;
        process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            record,
        )
        .await
        .expect("process XMLTV refresh");
        assert_eq!(job_status(&pool, created.refresh_job.id).await, "succeeded");
        let snapshot_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM source_snapshots WHERE epg_source_id = $1 AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("count active EPG snapshots");
        assert_eq!(snapshot_count, 1);

        delete_source(&pool, source_id).await;
        source_server.abort();
    }

    #[tokio::test]
    async fn worker_job_control_checkpoint_heartbeats_active_jobs() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let worker_id = "test-worker-checkpoint";
        let job = jobs
            .enqueue(&iptv_persistence::NewJob::immediate(
                "noop",
                serde_json::json!({}),
            ))
            .await
            .expect("enqueue job for checkpoint");
        force_running(&pool, job.id, worker_id).await;
        let control = WorkerJobControl {
            repository: jobs.clone(),
            job_id: job.id,
            worker_id: worker_id.to_owned(),
        };
        let progress = IngestProgress {
            phase: "download".to_owned(),
            downloaded_bytes: 1024,
            decoded_bytes: 0,
            records_seen: 0,
            records_prepared: 0,
        };
        control
            .checkpoint(&progress)
            .await
            .expect("checkpoint succeeds for active job");
        let heartbeat: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT heartbeat_at FROM jobs WHERE id = $1")
                .bind(job.id)
                .fetch_one(&pool)
                .await
                .expect("fetch heartbeat");
        assert!(heartbeat.is_some());
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    async fn worker_job_control_checkpoint_reports_cancellation() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let worker_id = "test-worker-cancelled";
        let job = jobs
            .enqueue(&iptv_persistence::NewJob::immediate(
                "noop",
                serde_json::json!({}),
            ))
            .await
            .expect("enqueue job for cancellation");
        force_running(&pool, job.id, worker_id).await;
        jobs.cancel(job.id).await.expect("cancel job");
        let control = WorkerJobControl {
            repository: jobs.clone(),
            job_id: job.id,
            worker_id: worker_id.to_owned(),
        };
        let progress = IngestProgress::default();
        let error = control
            .checkpoint(&progress)
            .await
            .expect_err("checkpoint reports cancellation");
        assert!(matches!(error, IngestError::Cancelled));
        delete_job(&pool, job.id).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn process_job_refreshes_xtream_short_epg_and_reconciles_catalog() {
        let Some((admin, database, schema)) = isolated_integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind Xtream short EPG fixture");
        let address = listener
            .local_addr()
            .expect("Xtream short EPG fixture address");
        let fixture = Arc::new(XtreamFixtureState::default());
        fixture.set_phase(XTREAM_PHASE_INITIAL);
        let fixture_server = tokio::spawn({
            let fixture = Arc::clone(&fixture);
            async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/player_api.php", get(xtream_fixture_response))
                        .with_state(fixture),
                )
                .await
                .expect("serve Xtream short EPG fixture");
            }
        });

        let pool = database.pool().clone();
        let master_key = MasterKey::from_bytes([11_u8; 32]);
        let jobs = JobRepository::new(pool.clone());
        let sources = SourceRepository::new(pool.clone(), master_key.clone());
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let worker_id = "test-worker-xtream-short-epg";
        let created = sources
            .create(
                &iptv_persistence::NewSource {
                    name: format!("Xtream short EPG test {}", Uuid::now_v7()),
                    kind: SourceKind::Xtream,
                    endpoint: format!(
                        "http://{address}/player_api.php?username=worker-user-canary&password=worker-password-canary&access_token=source-access-token-canary"
                    ),
                },
                "coverage-test",
            )
            .await
            .expect("create Xtream source for short EPG test");
        let source_id = created.source.id;

        // Process the initial refresh-source job. A successful Xtream refresh
        // must enqueue one refresh-xtream-short-epg job for the same source.
        process_refresh_job(
            &pool,
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            created.refresh_job.id,
        )
        .await;
        assert_eq!(
            job_status(&pool, created.refresh_job.id).await,
            "succeeded",
            "Xtream refresh failed: {:?}",
            job_last_error(&pool, created.refresh_job.id).await
        );
        assert_eq!(fixture.request_counts(), (1, 1, 1));

        // The successful Xtream refresh must queue exactly one short EPG job.
        let short_epg_job: JobRecord = sqlx::query_as::<_, JobRecord>(
            "SELECT * FROM jobs \
             WHERE kind = 'refresh-xtream-short-epg' \
               AND payload->>'sourceId' = $1 \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
        )
        .bind(source_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("short EPG job was enqueued after Xtream refresh");
        assert_eq!(short_epg_job.status, "queued");
        let short_epg_job_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM jobs \
             WHERE kind = 'refresh-xtream-short-epg' \
               AND payload->>'sourceId' = $1",
        )
        .bind(source_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("count short EPG jobs");
        assert_eq!(short_epg_job_count, 1);

        // Process the queued short EPG job. This exercises the full
        // refresh-xtream-short-epg handler end to end against the database.
        sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
            .bind(short_epg_job.id)
            .execute(&pool)
            .await
            .expect("set short EPG job max attempts");
        force_running(&pool, short_epg_job.id, worker_id).await;
        let short_epg_record = fetch_job(&pool, short_epg_job.id).await;
        if let Err(error) = process_job(
            &jobs,
            &sources,
            &snapshots,
            &catalog,
            &master_key,
            worker_id,
            short_epg_record,
        )
        .await
        {
            panic!("process_job failed for short EPG: {error:?}");
        }
        assert_eq!(
            job_status(&pool, short_epg_job.id).await,
            "succeeded",
            "short EPG refresh failed: {:?}",
            job_last_error(&pool, short_epg_job.id).await
        );
        assert!(
            fixture.short_epg_requests() >= 1,
            "short EPG fixture received no requests"
        );

        // The short EPG snapshot must be active and owned by the provider
        // account that the live-stream snapshot also owns.
        let short_epg_snapshot: (Uuid, String, String, Option<Uuid>) = sqlx::query_as(
            "SELECT id, kind, status, provider_account_id \
             FROM source_snapshots \
             WHERE provider_account_id = $1 AND kind = 'xtream-epg' AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("fetch active short EPG snapshot");
        assert_eq!(short_epg_snapshot.1, "xtream-epg");
        assert_eq!(short_epg_snapshot.2, "active");
        assert_eq!(short_epg_snapshot.3, Some(source_id));
        let short_epg_snapshot_id = short_epg_snapshot.0;
        let active_short_epg_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM source_snapshots \
             WHERE provider_account_id = $1 AND kind = 'xtream-epg' AND status = 'active'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("count active short EPG snapshots");
        assert_eq!(active_short_epg_count, 1);

        // The short EPG snapshot must stage one EPG channel and one programme.
        let epg_channel_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM epg_channels WHERE source_snapshot_id = $1")
                .bind(short_epg_snapshot_id)
                .fetch_one(&pool)
                .await
                .expect("count short EPG channels");
        assert_eq!(epg_channel_count, 1);
        let programme_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM programmes WHERE source_snapshot_id = $1")
                .bind(short_epg_snapshot_id)
                .fetch_one(&pool)
                .await
                .expect("count short EPG programmes");
        assert_eq!(programme_count, 1);
        let programme_title: String =
            sqlx::query_scalar("SELECT title FROM programmes WHERE source_snapshot_id = $1")
                .bind(short_epg_snapshot_id)
                .fetch_one(&pool)
                .await
                .expect("fetch short EPG programme title");
        assert_eq!(programme_title, "Broncos vs Chiefs");

        // The EPG reconciliation must link the short EPG channel to the
        // canonical channel that the live-stream snapshot produced. The
        // live-stream fixture exposes epg_channel_id "worker-epg-7", which
        // becomes the canonical key. The short EPG fixture reuses that
        // channel_id so the tvg-id pass applies the mapping.
        let channel_id: Uuid = sqlx::query_scalar(
            "SELECT c.id FROM channels c \
             WHERE c.provider_account_id = $1 AND c.canonical_key = 'worker-epg-7'",
        )
        .bind(source_id)
        .fetch_one(&pool)
        .await
        .expect("fetch canonical Xtream channel");
        let mapping_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM channel_epg_mappings WHERE channel_id = $1")
                .bind(channel_id)
                .fetch_one(&pool)
                .await
                .expect("count EPG mappings for canonical channel");
        assert!(
            mapping_count >= 1,
            "short EPG reconciliation did not map the canonical channel"
        );
        let mapped_programmes = catalog
            .list_programmes_for_channels(&[channel_id])
            .await
            .expect("list programmes for mapped channel");
        assert!(
            mapped_programmes
                .iter()
                .any(|programme| programme.title == "Broncos vs Chiefs"),
            "mapped channel did not expose the short EPG programme"
        );
        let now = chrono::Utc::now();
        assert!(
            mapped_programmes.iter().any(|programme| {
                programme.title == "Broncos vs Chiefs"
                    && programme.starts_at <= now
                    && now < programme.stops_at
            }),
            "mapped short EPG programme is not current"
        );

        delete_source(&pool, source_id).await;
        fixture_server.abort();
        drop(database);
        drop_isolated_schema(&admin, &schema).await;
    }
}
