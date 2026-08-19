use std::{env, net::SocketAddr, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use iptv_api::{AppConfig, AppState, RuntimeVersions, hash_admin_password};
use iptv_ingest::{
    DownloadRequest, EndpointProtector, IngestError, IngestFormat, IngestProgress, IngestRequest,
    IngestResult, Ingestor, JobControl, PgSnapshotStore, ProtectedEndpoint, SnapshotOwner,
};
use iptv_persistence::{
    CatalogRepository, Database, JobRecord, JobRepository, MasterKey, SourceKind, SourceRepository,
};
use reqwest::Url;
use tokio::{net::TcpListener, process::Command, signal, time::sleep};
use tracing::{error, info, warn};
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

#[derive(Debug)]
struct ServerEnvironment {
    bind: SocketAddr,
    public_base_url: String,
    output_token: String,
    admin_bootstrap_token: String,
    admin_password_hash: String,
    master_key: MasterKey,
    tuner_count: u16,
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
            hash_admin_password(&required_env_from(&mut lookup, "IPTV_ADMIN_PASSWORD")?)
                .map_err(anyhow::Error::msg)
                .context("hash administrator password")?
        }
    };

    Ok(ServerEnvironment {
        bind,
        public_base_url: lookup("IPTV_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:8080".to_owned()),
        output_token: required_env_from(&mut lookup, "IPTV_OUTPUT_TOKEN")?,
        admin_bootstrap_token: required_env_from(&mut lookup, "IPTV_ADMIN_BOOTSTRAP_TOKEN")?,
        admin_password_hash,
        master_key: MasterKey::from_base64(&required_env_from(&mut lookup, "IPTV_MASTER_KEY")?)
            .map_err(anyhow::Error::msg)
            .context("parse IPTV_MASTER_KEY")?,
        tuner_count: lookup("IPTV_TUNER_COUNT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
    })
}

async fn serve() -> Result<()> {
    let database = database().await?;
    let environment = server_environment()?;
    let bind = environment.bind;
    let config = environment.into_config(runtime_versions().await);
    let app = iptv_api::router(AppState::new(Some(database), config));
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
    lookup("IPTV_WORKER_ID").unwrap_or_else(|_| "worker-1".to_owned())
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
    loop {
        tokio::select! {
            () = shutdown_signal() => {
                info!(%worker_id, "job worker stopping");
                return Ok(());
            }
            result = repository.claim(&worker_id) => {
                match result {
                    Ok(Some(job)) => {
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
                    Ok(None) => sleep(Duration::from_millis(500)).await,
                    Err(error) => {
                        error!(%error, "job claim failed");
                        sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        }
    }
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
    if job.kind == "noop" {
        jobs.succeed(job.id, worker_id).await?;
        return Ok(());
    }
    if job.kind != "refresh-source" {
        warn!(job_id = %job.id, kind = %job.kind, "unsupported job kind");
        jobs.fail(
            &job,
            worker_id,
            "job handler is not registered",
            chrono::Utc::now() + chrono::Duration::seconds(30),
        )
        .await?;
        return Ok(());
    }

    match run_source_refresh(
        jobs, sources, snapshots, catalog, master_key, worker_id, &job,
    )
    .await
    {
        Ok(result) => {
            jobs.succeed(job.id, worker_id).await?;
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
            warn!(job_id = %job.id, error = summary, "source refresh failed");
            jobs.fail(
                &job,
                worker_id,
                summary,
                chrono::Utc::now() + chrono::Duration::seconds(30),
            )
            .await?;
        }
    }
    Ok(())
}

async fn run_source_refresh(
    jobs: &JobRepository,
    sources: &SourceRepository,
    snapshots: &PgSnapshotStore,
    catalog: &CatalogRepository,
    master_key: &MasterKey,
    worker_id: &str,
    job: &JobRecord,
) -> std::result::Result<IngestResult, RefreshError> {
    let source_id = source_id_from_payload(&job.payload).ok_or(RefreshError::InvalidPayload)?;
    let source = sources
        .load_for_job(source_id)
        .await
        .map_err(|_| RefreshError::SourceLoad)?;
    let (owner, format) = refresh_target(source.kind, source.id)?;
    let endpoint = Url::parse(&source.endpoint).map_err(|_| RefreshError::InvalidEndpoint)?;
    let control = WorkerJobControl {
        repository: jobs.clone(),
        job_id: job.id,
        worker_id: worker_id.to_owned(),
    };
    let protector = StreamEndpointProtector {
        master_key: master_key.clone(),
        associated_data: format!("iptv-provider-stream:v1:{}", source.id),
    };
    let request = IngestRequest {
        owner,
        format,
        download: DownloadRequest::new(endpoint),
        source_timezone: source.timezone,
        xtream_stream_endpoint: None,
    };
    let result = Ingestor::new(protector, control, snapshots.clone())
        .run(&request)
        .await
        .map_err(RefreshError::Ingest)?;
    // Best-effort catalog reconciliation. A snapshot is already active, so a
    // reconciliation failure leaves the previous catalog in place and is logged.
    match source.kind {
        SourceKind::M3u => {
            if let Err(error) = catalog.reconcile_provider_account(source.id).await {
                warn!(job_id = %job.id, source_id = %source.id, error = %error, "canonical channel reconciliation failed");
            }
            if let Err(error) = catalog.reconcile_epg_mappings().await {
                warn!(job_id = %job.id, error = %error, "epg mapping reconciliation failed");
            }
        }
        SourceKind::Xmltv => {
            if let Err(error) = catalog.reconcile_epg_mappings().await {
                warn!(job_id = %job.id, error = %error, "epg mapping reconciliation failed");
            }
        }
        SourceKind::Xtream | SourceKind::NetworkTuner => {}
    }
    Ok(result)
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
        SourceKind::Xtream | SourceKind::NetworkTuner => Err(RefreshError::UnsupportedSource),
    }
}

#[derive(Debug)]
enum RefreshError {
    InvalidPayload,
    SourceLoad,
    InvalidEndpoint,
    UnsupportedSource,
    Ingest(IngestError),
}

impl RefreshError {
    const fn persisted_summary(&self) -> &'static str {
        match self {
            Self::InvalidPayload => "refresh job payload is invalid",
            Self::SourceLoad => "source configuration could not be loaded",
            Self::InvalidEndpoint => "source endpoint is invalid",
            Self::UnsupportedSource => "source type is not supported by the refresh worker",
            Self::Ingest(error) => error.persisted_summary(),
        }
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
        let progress = serde_json::to_value(progress)
            .map_err(|_| IngestError::InvalidRequest("job progress could not be serialized"))?;
        self.repository
            .heartbeat(self.job_id, &self.worker_id, &progress)
            .await
            .map_err(|_| IngestError::OwnershipLost)
    }
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
    use std::collections::HashMap;

    const MASTER_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

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
            ("IPTV_OUTPUT_TOKEN", "output-secret"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap-secret"),
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
    fn server_environment_uses_stable_defaults() {
        let environment = server_environment_from(lookup(&required_settings())).expect("settings");
        assert_eq!(environment.bind, "0.0.0.0:8081".parse().expect("address"));
        assert_eq!(environment.public_base_url, "http://localhost:8080");
        assert_eq!(environment.output_token, "output-secret");
        assert_eq!(environment.admin_bootstrap_token, "bootstrap-secret");
        assert_eq!(environment.admin_password_hash, "already-hashed");
        assert_eq!(environment.tuner_count, 1);
    }

    #[test]
    fn server_environment_accepts_all_overrides() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_BIND", "127.0.0.1:9000"),
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_PUBLIC_BASE_URL", "https://iptv.example.test/base"),
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
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
                ("IPTV_OUTPUT_TOKEN", "output"),
                ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
                ("IPTV_MASTER_KEY", MASTER_KEY),
                ("IPTV_TUNER_COUNT", value),
            ]))
            .expect("invalid tuner count defaults");
            assert_eq!(environment.tuner_count, 1);
        }
    }

    #[test]
    fn zero_tuners_remains_accepted_for_backward_compatibility() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
            ("IPTV_MASTER_KEY", MASTER_KEY),
            ("IPTV_TUNER_COUNT", "0"),
        ]))
        .expect("zero was accepted by the original parser");
        assert_eq!(environment.tuner_count, 0);
    }

    #[test]
    fn plaintext_password_is_hashed_when_precomputed_hash_is_absent_or_blank() {
        for optional_hash in [None, Some("  ")] {
            let mut entries = vec![
                ("IPTV_ADMIN_PASSWORD", "correct horse battery staple"),
                ("IPTV_OUTPUT_TOKEN", "output"),
                ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
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
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
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
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
            ("IPTV_MASTER_KEY", invalid),
        ]))
        .expect_err("invalid master key");
        assert!(error.to_string().contains("parse IPTV_MASTER_KEY"));
        assert!(!error.to_string().contains(invalid));
    }

    #[test]
    fn missing_plaintext_password_is_reported_when_hash_is_unavailable() {
        let error = server_environment_from(lookup(&[
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
        ]))
        .expect_err("missing password");
        assert!(error.to_string().contains("IPTV_ADMIN_PASSWORD"));
    }

    #[test]
    fn environment_converts_to_api_config_without_data_loss() {
        let environment = server_environment_from(lookup(&required_settings())).expect("settings");
        let versions = RuntimeVersions {
            gateway: "gateway-version".to_owned(),
            ffmpeg: Some("ffmpeg-version".to_owned()),
            vlc: Some("vlc-version".to_owned()),
        };
        let config = environment.into_config(versions);
        assert_eq!(config.public_base_url, "http://localhost:8080");
        assert_eq!(config.output_token, "output-secret");
        assert_eq!(config.admin_bootstrap_token, "bootstrap-secret");
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
    fn worker_id_uses_override_or_default() {
        assert_eq!(worker_id_from(lookup(&[])), "worker-1");
        assert_eq!(
            worker_id_from(lookup(&[("IPTV_WORKER_ID", "worker-blue")])),
            "worker-blue"
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
        assert!(matches!(
            refresh_target(SourceKind::Xtream, source_id),
            Err(RefreshError::UnsupportedSource)
        ));
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
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect("IPv6 bind");
        assert!(environment.bind.is_ipv6());
    }

    #[test]
    fn maximum_tuner_count_is_supported() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
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
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
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
            ("IPTV_OUTPUT_TOKEN", "output"),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", "bootstrap"),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect("blank public URL");
        assert!(environment.public_base_url.is_empty());
    }

    #[test]
    fn explicitly_blank_required_secrets_remain_accepted() {
        let environment = server_environment_from(lookup(&[
            ("IPTV_ADMIN_PASSWORD_HASH", "hash"),
            ("IPTV_OUTPUT_TOKEN", ""),
            ("IPTV_ADMIN_BOOTSTRAP_TOKEN", ""),
            ("IPTV_MASTER_KEY", MASTER_KEY),
        ]))
        .expect("presence, rather than content, is required");
        assert!(environment.output_token.is_empty());
        assert!(environment.admin_bootstrap_token.is_empty());
    }

    #[test]
    fn blank_worker_id_is_preserved() {
        assert_eq!(worker_id_from(lookup(&[("IPTV_WORKER_ID", "")])), "");
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
                "IPTV_OUTPUT_TOKEN" => Ok("output".to_owned()),
                "IPTV_ADMIN_BOOTSTRAP_TOKEN" => Ok("bootstrap".to_owned()),
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
    }

    async fn integration_database() -> Option<Database> {
        let url = std::env::var("IPTV_TEST_DATABASE_URL").ok()?;
        let database = Database::connect(&url, 4)
            .await
            .expect("connect to test database");
        database.migrate().await.expect("run migrations");
        Some(database)
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

    #[tokio::test]
    async fn process_job_fails_refresh_source_for_unsupported_xtream_source() {
        let Some(database) = integration_database().await else {
            eprintln!("IPTV_TEST_DATABASE_URL is unset; skipping integration test");
            return;
        };
        let pool = database.pool().clone();
        let jobs = JobRepository::new(pool.clone());
        let master_key = MasterKey::from_bytes([1_u8; 32]);
        let sources = SourceRepository::new(pool.clone(), master_key.clone());
        let snapshots = PgSnapshotStore::new(pool.clone());
        let catalog = CatalogRepository::new(pool.clone());
        let worker_id = "test-worker-xtream";
        let suffix = Uuid::now_v7();
        let created = sources
            .create(
                &iptv_persistence::NewSource {
                    name: format!("Xtream coverage test {suffix}"),
                    kind: SourceKind::Xtream,
                    endpoint: "https://provider.test/live".to_owned(),
                },
                "coverage-test",
            )
            .await
            .expect("create xtream source");
        let source_id = created.source.id;
        let job_id = created.refresh_job.id;
        sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
            .bind(job_id)
            .execute(&pool)
            .await
            .expect("set max attempts");
        force_running(&pool, job_id, worker_id).await;
        let record = fetch_job(&pool, job_id).await;
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
        .expect("process xtream refresh");
        assert_eq!(job_status(&pool, job_id).await, "failed");
        let error = job_last_error(&pool, job_id)
            .await
            .expect("error was persisted");
        assert!(error.contains("source type is not supported"));
        delete_source(&pool, source_id).await;
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
}
