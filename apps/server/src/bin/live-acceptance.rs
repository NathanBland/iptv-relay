//! Live provider media acceptance runner.
//!
//! This binary supports two modes:
//!
//! - Gateway mode: uses the gateway output routes (`/out/{token}/...`) and
//!   drives the admin API to seed the gateway from a test provider. This is
//!   the real gateway live acceptance path. Select this mode by setting
//!   `IPTV_GATEWAY_URL`.
//! - Direct mode: downloads a real M3U playlist from `IPTV_TEST_M3U_URL` and
//!   streams directly from the provider. This is the legacy path.
//!
//! Gateway mode:
//! 1. Waits for the gateway and the test provider to become healthy.
//! 2. Creates (or reuses) an M3U source that points at the test provider.
//! 3. Sets the source max connections to the provider cap.
//! 4. Triggers a source sync and waits for three channels to appear.
//! 5. Downloads the gateway playlist and selects three stable channels.
//! 6. Runs three viewers (one per channel) for the configured duration.
//! 7. Runs six viewers (two per channel) for the configured duration.
//! 8. Asserts three upstream sessions, no drops after warmup, and final-viewer
//!    slot release.
//! 9. Optionally runs a 30-minute soak when `LIVE_ACCEPTANCE_SOAK=1`.

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, future::join_all};
use iptv_media::MPEG_TS_PACKET_SIZE;
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};

const DEFAULT_CHANNELS: usize = 3;
const DEFAULT_PROVIDER_CAP: usize = 3;
const DEFAULT_SECONDS: u64 = 120;
const DEFAULT_SOAK_SECONDS: u64 = 30 * 60;
const SOURCE_NAME: &str = "live-acceptance-provider";

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("IPTV_GATEWAY_URL").is_some() {
        run_gateway_acceptance().await
    } else {
        run_direct_acceptance().await
    }
}

// ---------------------------------------------------------------------------
// Gateway mode
// ---------------------------------------------------------------------------

struct GatewayConfig {
    gateway_url: String,
    output_token: String,
    bootstrap_token: String,
    provider_url: String,
    channels: usize,
    provider_cap: usize,
    seconds: u64,
    soak_enabled: bool,
    soak_seconds: u64,
}

impl std::fmt::Debug for GatewayConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayConfig")
            .field("gateway_url", &self.gateway_url)
            .field("output_token", &"<redacted>")
            .field("bootstrap_token", &"<redacted>")
            .field("provider_url", &self.provider_url)
            .field("channels", &self.channels)
            .field("provider_cap", &self.provider_cap)
            .field("seconds", &self.seconds)
            .field("soak_enabled", &self.soak_enabled)
            .field("soak_seconds", &self.soak_seconds)
            .finish()
    }
}

impl GatewayConfig {
    fn from_env() -> Result<Self> {
        let gateway_url = env_required("IPTV_GATEWAY_URL")?
            .trim_end_matches('/')
            .to_owned();
        let output_token = env_required("IPTV_OUTPUT_TOKEN")?;
        let bootstrap_token = env_required("IPTV_ADMIN_BOOTSTRAP_TOKEN")?;
        let provider_url = env_required("IPTV_TEST_PROVIDER_URL")?
            .trim_end_matches('/')
            .to_owned();
        let channels = env_usize("LIVE_ACCEPTANCE_CHANNELS", DEFAULT_CHANNELS)?;
        let provider_cap = env_usize("LIVE_ACCEPTANCE_PROVIDER_CAP", DEFAULT_PROVIDER_CAP)?;
        let seconds = env_u64("LIVE_ACCEPTANCE_SECONDS", DEFAULT_SECONDS)?;
        let soak_enabled = env_bool("LIVE_ACCEPTANCE_SOAK", false)?;
        let soak_seconds = env_u64("LIVE_ACCEPTANCE_SOAK_SECONDS", DEFAULT_SOAK_SECONDS)?;
        ensure!(
            channels >= 1 && channels <= provider_cap,
            "LIVE_ACCEPTANCE_CHANNELS must be between 1 and the provider cap ({provider_cap})"
        );
        Ok(Self {
            gateway_url,
            output_token,
            bootstrap_token,
            provider_url,
            channels,
            provider_cap,
            seconds,
            soak_enabled,
            soak_seconds,
        })
    }
}

#[allow(clippy::too_many_lines, clippy::duration_suboptimal_units)]
async fn run_gateway_acceptance() -> Result<()> {
    let config = GatewayConfig::from_env()?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let stream_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // Long-lived MPEG-TS streams must not time out while idle. Use a
        // generous upper bound instead of disabling the timeout entirely.
        .timeout(Duration::from_secs(24 * 60 * 60))
        .build()?;

    wait_for_gateway(&client, &config.gateway_url).await?;
    wait_for_test_provider(&client, &config.provider_url).await?;
    reset_provider(&client, &config.provider_url).await?;

    let source_id = ensure_source(&client, &config).await?;
    let channels = wait_for_channels(&client, &config, source_id).await?;
    ensure!(
        channels.len() >= config.channels,
        "the gateway playlist exposes {} channels, but {} are required",
        channels.len(),
        config.channels
    );

    let selected = select_stable_channels(&stream_client, &config, &channels).await?;
    ensure!(
        selected.len() == config.channels,
        "only {} channels delivered stable MPEG-TS, but {} are required",
        selected.len(),
        config.channels
    );

    // The probes open transient upstream sessions. Wait for them to drain so
    // each scenario starts from a clean provider state.
    wait_for_no_upstreams(&client, &config.provider_url).await?;

    // Scenario A: one viewer per channel.
    run_scenario(
        &stream_client,
        &client,
        &config,
        &selected,
        1,
        config.seconds,
        "three viewers",
    )
    .await?;

    // Scenario B: two viewers per channel (six viewers total).
    run_scenario(
        &stream_client,
        &client,
        &config,
        &selected,
        2,
        config.seconds,
        "six viewers",
    )
    .await?;

    if config.soak_enabled {
        run_scenario(
            &stream_client,
            &client,
            &config,
            &selected,
            2,
            config.soak_seconds,
            "soak",
        )
        .await?;
    }

    println!(
        "live gateway acceptance passed: {} channels, {}s scenarios, soak={}",
        config.channels, config.seconds, config.soak_enabled
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct SourceResponse {
    id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct ChannelEntry {
    id: uuid::Uuid,
}

async fn wait_for_gateway(client: &reqwest::Client, gateway_url: &str) -> Result<()> {
    #[allow(clippy::duration_suboptimal_units)]
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if client
            .get(format!("{gateway_url}/health/live"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the gateway did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_test_provider(client: &reqwest::Client, provider_url: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if client
            .get(format!("{provider_url}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the test provider did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn reset_provider(client: &reqwest::Client, provider_url: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let response = client
            .post(format!("{provider_url}/reset"))
            .send()
            .await
            .context("reset the test provider")?;
        if response.status().is_success() {
            return Ok(());
        }
        // A 409 conflict means a previous run left streams open. Retry until
        // the upstream sessions drain.
        ensure!(
            Instant::now() < deadline,
            "the test provider could not be reset"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Create or reuse the M3U source and set its max connections to the cap.
async fn ensure_source(client: &reqwest::Client, config: &GatewayConfig) -> Result<uuid::Uuid> {
    if let Some(source_id) = find_source_by_name(client, config).await? {
        set_source_max_connections(client, config, source_id).await?;
        return Ok(source_id);
    }
    let response = client
        .post(format!("{}/api/v1/sources", config.gateway_url))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.bootstrap_token),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(&serde_json::json!({
            "name": SOURCE_NAME,
            "kind": "M3U",
            "endpoint": format!("{}/playlist.m3u", config.provider_url),
        }))?)
        .send()
        .await
        .context("create the gateway source")?;
    if !response.status().is_success() {
        bail!(
            "the gateway rejected source creation: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    let body = response
        .text()
        .await
        .context("read the source response body")?;
    let source: SourceResponse =
        serde_json::from_str(&body).with_context(|| format!("parse source response: {body}"))?;
    let source_id = uuid::Uuid::parse_str(&source.id).context("parse source id")?;
    set_source_max_connections(client, config, source_id).await?;
    Ok(source_id)
}

async fn find_source_by_name(
    client: &reqwest::Client,
    config: &GatewayConfig,
) -> Result<Option<uuid::Uuid>> {
    let response = client
        .get(format!("{}/api/v1/sources", config.gateway_url))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.bootstrap_token),
        )
        .send()
        .await
        .context("list gateway sources")?;
    ensure!(
        response.status().is_success(),
        "the gateway rejected the source list: {}",
        response.status()
    );
    let body = response.text().await.context("read the source list body")?;
    let sources: Vec<SourceResponse> =
        serde_json::from_str(&body).with_context(|| format!("parse source list: {body}"))?;
    Ok(sources
        .into_iter()
        .find(|source| source.name == SOURCE_NAME)
        .and_then(|source| uuid::Uuid::parse_str(&source.id).ok()))
}

async fn set_source_max_connections(
    client: &reqwest::Client,
    config: &GatewayConfig,
    source_id: uuid::Uuid,
) -> Result<()> {
    let response = client
        .patch(format!(
            "{}/api/v1/sources/{}",
            config.gateway_url, source_id
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.bootstrap_token),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(&serde_json::json!({
            "maxConnections": config.provider_cap,
        }))?)
        .send()
        .await
        .context("set the source max connections")?;
    ensure!(
        response.status().is_success(),
        "the gateway rejected the max connections update: {} {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
    Ok(())
}

/// Trigger a source sync and wait until the gateway playlist exposes enough
/// channels.
async fn wait_for_channels(
    client: &reqwest::Client,
    config: &GatewayConfig,
    source_id: uuid::Uuid,
) -> Result<Vec<ChannelEntry>> {
    // Trigger a sync. A 409 conflict means a sync is already active, which is
    // acceptable.
    let sync_response = client
        .post(format!(
            "{}/api/v1/sources/{}/sync",
            config.gateway_url, source_id
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.bootstrap_token),
        )
        .send()
        .await
        .context("trigger the source sync")?;
    ensure!(
        sync_response.status().is_success() || sync_response.status().as_u16() == 409,
        "the gateway rejected the source sync: {}",
        sync_response.status()
    );

    #[allow(clippy::duration_suboptimal_units)]
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let playlist = download_gateway_playlist(client, config).await?;
        let channels = parse_gateway_playlist(&playlist, config);
        if channels.len() >= config.channels {
            return Ok(channels);
        }
        ensure!(
            Instant::now() < deadline,
            "the gateway did not expose {} channels within 120 seconds (found {})",
            config.channels,
            channels.len()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn download_gateway_playlist(
    client: &reqwest::Client,
    config: &GatewayConfig,
) -> Result<String> {
    client
        .get(format!(
            "{}/out/{}/playlist.m3u",
            config.gateway_url, config.output_token
        ))
        .send()
        .await
        .context("download the gateway playlist")?
        .error_for_status()?
        .text()
        .await
        .context("read the gateway playlist body")
}

fn parse_gateway_playlist(playlist: &str, config: &GatewayConfig) -> Vec<ChannelEntry> {
    let mut entries = Vec::new();
    for line in playlist.lines().map(str::trim) {
        if !line.starts_with("http") {
            continue;
        }
        let Some(channel_id) = extract_channel_id(line) else {
            continue;
        };
        entries.push(ChannelEntry { id: channel_id });
        if entries.len() >= config.channels {
            break;
        }
    }
    entries
}

/// Build the gateway output route for a channel. The playlist may emit URLs
/// with `IPTV_PUBLIC_BASE_URL` (for example `http://localhost:8080`), which is
/// unreachable from inside the compose network. Rebuild the URL from the
/// configured gateway URL so viewers always reach the gateway.
fn gateway_stream_url(config: &GatewayConfig, channel_id: uuid::Uuid) -> String {
    format!(
        "{}/out/{}/stream/{}.ts",
        config.gateway_url, config.output_token, channel_id
    )
}

/// Extract the channel UUID from a gateway stream URL.
/// The gateway emits `{base}/out/{token}/stream/{channel_id}.ts`.
fn extract_channel_id(stream_url: &str) -> Option<uuid::Uuid> {
    let path = stream_url.split('?').next().unwrap_or(stream_url);
    let file_name = path.rsplit('/').next()?;
    let stem = file_name.strip_suffix(".ts")?;
    uuid::Uuid::parse_str(stem).ok()
}

/// Probe each channel through the gateway output route and keep the channels
/// that deliver MPEG-TS within the warmup window.
async fn select_stable_channels(
    stream_client: &reqwest::Client,
    config: &GatewayConfig,
    channels: &[ChannelEntry],
) -> Result<Vec<ChannelEntry>> {
    let mut stable = Vec::with_capacity(config.channels);
    for channel in channels {
        if stable.len() == config.channels {
            break;
        }
        let stream_url = gateway_stream_url(config, channel.id);
        if probe_gateway_stream(stream_client, &stream_url).await {
            stable.push(ChannelEntry { id: channel.id });
        }
    }
    Ok(stable)
}

async fn probe_gateway_stream(client: &reqwest::Client, stream_url: &str) -> bool {
    let Ok(response) = client.get(stream_url).send().await else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let mut stream = response.bytes_stream();
    if let Some(Ok(chunk)) = stream.next().await
        && !chunk.is_empty()
        && chunk[0] == 0x47
    {
        return true;
    }
    false
}

#[derive(Debug)]
struct Viewer {
    bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    task: JoinHandle<Result<()>>,
}

impl Viewer {
    fn stop(self) {
        self.task.abort();
    }
}

/// Run one scenario: open `viewers_per_channel` viewers per channel, warm up,
/// assert three upstream sessions, soak, assert no drops, then release and
/// assert final slot release.
async fn run_scenario(
    stream_client: &reqwest::Client,
    client: &reqwest::Client,
    config: &GatewayConfig,
    channels: &[ChannelEntry],
    viewers_per_channel: usize,
    seconds: u64,
    label: &str,
) -> Result<()> {
    reset_provider(client, &config.provider_url).await?;

    let mut viewers = Vec::with_capacity(channels.len() * viewers_per_channel);
    for channel in channels {
        let gate = std::sync::Arc::new(tokio::sync::Barrier::new(viewers_per_channel));
        let stream_url = gateway_stream_url(config, channel.id);
        let opens = (0..viewers_per_channel).map(|_| {
            let stream_client = stream_client.clone();
            let stream_url = stream_url.clone();
            let gate = std::sync::Arc::clone(&gate);
            async move {
                gate.wait().await;
                open_viewer(&stream_client, &stream_url).await
            }
        });
        let opened = join_all(opens).await;
        for viewer in opened {
            viewers
                .push(viewer.with_context(|| {
                    format!("open a gateway viewer for channel {}", channel.id)
                })?);
        }
    }

    wait_for_warmup(&viewers, Duration::from_secs(30)).await?;
    let metrics = provider_metrics(client, &config.provider_url).await?;
    assert_scenario_metrics(&metrics, config, label, "warmup")?;

    let before = byte_counts(&viewers);
    let end = Instant::now() + Duration::from_secs(seconds);
    tokio::time::sleep_until(end).await;
    let after = byte_counts(&viewers);
    ensure!(
        after
            .iter()
            .zip(before)
            .all(|(after, before)| *after > before),
        "one or more {label} viewers stopped receiving media during the soak"
    );
    ensure!(
        viewers.iter().all(|viewer| !viewer.task.is_finished()),
        "one or more {label} viewers dropped during the soak"
    );

    let metrics = provider_metrics(client, &config.provider_url).await?;
    assert_scenario_metrics(&metrics, config, label, "soak")?;

    for viewer in viewers {
        viewer.stop();
    }
    wait_for_no_upstreams(client, &config.provider_url).await?;
    let final_metrics = provider_metrics(client, &config.provider_url).await?;
    ensure!(
        final_metrics.active_streams == 0,
        "the final {label} viewers did not release the upstream slots"
    );
    ensure!(
        final_metrics.rejected_connections == 0,
        "the {label} scenario rejected {} provider connections",
        final_metrics.rejected_connections
    );
    println!(
        "{label} scenario passed: {} channels, {} viewers, {seconds}s",
        config.channels,
        config.channels * viewers_per_channel
    );
    Ok(())
}

async fn open_viewer(client: &reqwest::Client, stream_url: &str) -> Result<Viewer> {
    let response = client
        .get(stream_url)
        .send()
        .await
        .context("open the gateway stream")?
        .error_for_status()?;
    let stream = response.bytes_stream();
    let bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = std::sync::Arc::clone(&bytes);
    let task = tokio::spawn(async move {
        let mut stream = Box::pin(stream);
        let mut first_chunk = true;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read a chunk from the gateway stream")?;
            ensure!(!chunk.is_empty(), "the gateway stream sent an empty chunk");
            if first_chunk {
                ensure!(
                    chunk[0] == 0x47,
                    "the gateway stream does not start with an MPEG-TS sync byte"
                );
                first_chunk = false;
            }
            observed.fetch_add(
                u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        bail!("the gateway viewer disconnected before cancellation")
    });
    Ok(Viewer { bytes, task })
}

async fn wait_for_warmup(viewers: &[Viewer], timeout: Duration) -> Result<()> {
    let minimum = u64::try_from(MPEG_TS_PACKET_SIZE * 20).unwrap_or(u64::MAX);
    let deadline = Instant::now() + timeout;
    loop {
        if viewers.iter().all(|viewer| {
            viewer.bytes.load(std::sync::atomic::Ordering::Relaxed) >= minimum
                && !viewer.task.is_finished()
        }) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the gateway viewers did not receive MPEG-TS in time"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn byte_counts(viewers: &[Viewer]) -> Vec<u64> {
    viewers
        .iter()
        .map(|viewer| viewer.bytes.load(std::sync::atomic::Ordering::Relaxed))
        .collect()
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ProviderMetrics {
    active_streams: usize,
    high_water: usize,
    max_connections: usize,
    rejected_connections: u64,
}

async fn provider_metrics(client: &reqwest::Client, provider_url: &str) -> Result<ProviderMetrics> {
    let body = client
        .get(format!("{provider_url}/metrics.json"))
        .send()
        .await
        .context("read the provider metrics")?
        .error_for_status()?
        .text()
        .await?;
    serde_json::from_str(&body).with_context(|| format!("parse provider metrics: {body}"))
}

/// Assert that the provider metrics match the acceptance contract for one
/// scenario. The provider must report the configured channel count as active
/// streams, the configured cap as `max_connections`, a high-water mark equal
/// to the channel count (and never above the cap), and zero rejected
/// connections.
fn assert_scenario_metrics(
    metrics: &ProviderMetrics,
    config: &GatewayConfig,
    label: &str,
    phase: &str,
) -> Result<()> {
    ensure!(
        metrics.max_connections == config.provider_cap,
        "the {label} {phase} provider max_connections is {}, but the configured cap is {}",
        metrics.max_connections,
        config.provider_cap
    );
    ensure!(
        metrics.active_streams == config.channels,
        "the {label} {phase} provider reported {} active streams, but {} are expected",
        metrics.active_streams,
        config.channels
    );
    ensure!(
        metrics.high_water == config.channels,
        "the {label} {phase} provider high_water is {}, but {} channels were opened",
        metrics.high_water,
        config.channels
    );
    ensure!(
        metrics.high_water <= metrics.max_connections,
        "the {label} {phase} provider high_water {} exceeded the cap {}",
        metrics.high_water,
        metrics.max_connections
    );
    ensure!(
        metrics.rejected_connections == 0,
        "the {label} {phase} provider rejected {} connections",
        metrics.rejected_connections
    );
    Ok(())
}

async fn wait_for_no_upstreams(client: &reqwest::Client, provider_url: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if provider_metrics(client, provider_url).await?.active_streams == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the final viewers did not close the upstream sessions within 15 seconds"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set"))
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    parse_env_usize(std::env::var(name).ok().as_deref(), name, default)
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    parse_env_u64(std::env::var(name).ok().as_deref(), name, default)
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    parse_env_bool(std::env::var(name).ok().as_deref(), name, default)
}

fn parse_env_usize(value: Option<&str>, name: &str, default: usize) -> Result<usize> {
    match value {
        Some(raw) => raw
            .parse::<usize>()
            .with_context(|| format!("{name} must be a non-negative integer, got {raw:?}")),
        None => Ok(default),
    }
}

fn parse_env_u64(value: Option<&str>, name: &str, default: u64) -> Result<u64> {
    match value {
        Some(raw) => raw
            .parse::<u64>()
            .with_context(|| format!("{name} must be a non-negative integer, got {raw:?}")),
        None => Ok(default),
    }
}

fn parse_env_bool(value: Option<&str>, name: &str, default: bool) -> Result<bool> {
    match value {
        Some(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                _ => {
                    bail!("{name} must be one of 1, true, yes, on, 0, false, no, off, got {raw:?}")
                }
            }
        }
        None => Ok(default),
    }
}

// ---------------------------------------------------------------------------
// Direct mode (legacy)
// ---------------------------------------------------------------------------

async fn run_direct_acceptance() -> Result<()> {
    let m3u_url = std::env::var("IPTV_TEST_M3U_URL")
        .context("IPTV_TEST_M3U_URL must be set to a live M3U playlist URL")?;
    let seconds = env_u64("LIVE_ACCEPTANCE_SECONDS", 30)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(seconds + 30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;

    #[allow(clippy::duration_suboptimal_units)]
    let playlist_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;
    let playlist = playlist_client
        .get(&m3u_url)
        .send()
        .await
        .context("download M3U playlist")?
        .error_for_status()?
        .text()
        .await
        .context("read M3U playlist body")?;

    ensure!(!playlist.is_empty(), "the M3U playlist is empty");

    let stream_urls = collect_ts_stream_urls(&playlist);
    ensure!(
        !stream_urls.is_empty(),
        "the M3U playlist contains no .ts stream URL"
    );

    let stream_url = find_active_stream(&stream_urls)
        .await
        .context("no live stream delivered data within 60 seconds")?;

    let (packet_count, total_bytes) = read_stream(&client, &stream_url, seconds).await?;

    ensure!(
        packet_count > 0,
        "the live stream delivered no MPEG-TS packets"
    );
    ensure!(
        total_bytes >= MPEG_TS_PACKET_SIZE as u64,
        "the live stream delivered insufficient data: {total_bytes} bytes"
    );

    println!("live acceptance passed: {packet_count} packets, {total_bytes} bytes, {seconds}s");
    Ok(())
}

fn collect_ts_stream_urls(playlist: &str) -> Vec<String> {
    playlist
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("http")
                && std::path::Path::new(line)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
        })
        .map(String::from)
        .collect()
}

async fn find_active_stream(urls: &[String]) -> Option<String> {
    #[allow(clippy::duration_suboptimal_units)]
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    for url in urls {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        if let Some(valid_url) = probe_stream(url).await {
            return Some(valid_url);
        }
    }
    None
}

async fn probe_stream(url: &str) -> Option<String> {
    let probe_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .ok()?;
    let response = probe_client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut stream = response.bytes_stream();
    if let Some(Ok(chunk)) = stream.next().await
        && !chunk.is_empty()
        && chunk[0] == 0x47
    {
        return Some(url.to_owned());
    }
    None
}

async fn read_stream(
    client: &reqwest::Client,
    stream_url: &str,
    seconds: u64,
) -> Result<(u64, u64)> {
    let response = client
        .get(stream_url)
        .send()
        .await
        .context("open the live stream")?
        .error_for_status()?;

    let mut stream = response.bytes_stream();
    let mut total_bytes: u64 = 0;
    let mut packet_count: u64 = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read a chunk from the live stream")?;
        ensure!(!chunk.is_empty(), "the live stream sent an empty chunk");

        if total_bytes == 0 {
            ensure!(
                chunk[0] == 0x47,
                "the live stream does not start with an MPEG-TS sync byte"
            );
        }

        packet_count += u64::try_from(chunk.len() / MPEG_TS_PACKET_SIZE).unwrap_or(0);
        total_bytes += u64::try_from(chunk.len()).unwrap_or(u64::MAX);

        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }

    Ok((packet_count, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(channels: usize, provider_cap: usize) -> GatewayConfig {
        GatewayConfig {
            gateway_url: "http://gateway.test".to_owned(),
            output_token: "token".to_owned(),
            bootstrap_token: "bootstrap".to_owned(),
            provider_url: "http://provider.test".to_owned(),
            channels,
            provider_cap,
            seconds: DEFAULT_SECONDS,
            soak_enabled: false,
            soak_seconds: DEFAULT_SOAK_SECONDS,
        }
    }

    fn metrics(active: usize, high_water: usize, max: usize, rejected: u64) -> ProviderMetrics {
        ProviderMetrics {
            active_streams: active,
            high_water,
            max_connections: max,
            rejected_connections: rejected,
        }
    }

    #[test]
    fn extract_channel_id_parses_gateway_stream_url() {
        let url = "http://gateway:8080/out/abc/stream/01928374-5678-7000-8000-123456789abc.ts";
        let id = extract_channel_id(url).expect("a gateway stream URL has a channel id");
        assert_eq!(
            id,
            uuid::Uuid::parse_str("01928374-5678-7000-8000-123456789abc").unwrap()
        );
    }

    #[test]
    fn extract_channel_id_ignores_query_strings_and_non_uuid_paths() {
        assert!(extract_channel_id("http://gateway:8080/out/abc/stream/not-a-uuid.ts").is_none());
        let id = extract_channel_id(
            "http://gateway:8080/out/abc/stream/01928374-5678-7000-8000-123456789abc.ts?foo=bar",
        )
        .expect("query strings do not block channel id extraction");
        assert_eq!(
            id,
            uuid::Uuid::parse_str("01928374-5678-7000-8000-123456789abc").unwrap()
        );
    }

    #[test]
    fn gateway_stream_url_uses_configured_gateway_not_playlist_host() {
        let config = config(3, 3);
        let id = uuid::Uuid::parse_str("01928374-5678-7000-8000-123456789abc").unwrap();
        let url = gateway_stream_url(&config, id);
        assert_eq!(
            url,
            "http://gateway.test/out/token/stream/01928374-5678-7000-8000-123456789abc.ts"
        );
    }

    #[test]
    fn parse_gateway_playlist_collects_channel_entries() {
        let config = config(3, 3);
        let playlist = "\
#EXTM3U x-tvg-url=\"http://gateway:8080/out/token/xmltv.xml\"
#EXTINF:-1 tvg-id=\"a\" tvg-name=\"A\",A
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000001.ts
#EXTINF:-1 tvg-id=\"b\" tvg-name=\"B\",B
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000002.ts
#EXTINF:-1 tvg-id=\"c\" tvg-name=\"C\",C
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000003.ts
";
        let channels = parse_gateway_playlist(playlist, &config);
        assert_eq!(channels.len(), 3);
        assert_eq!(
            channels[0].id,
            uuid::Uuid::parse_str("01928374-5678-7000-8000-000000000001").unwrap()
        );
    }

    #[test]
    fn parse_gateway_playlist_stops_at_requested_channel_count() {
        let config = config(2, 3);
        let playlist = "\
#EXTM3U
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000001.ts
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000002.ts
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000003.ts
";
        let channels = parse_gateway_playlist(playlist, &config);
        assert_eq!(channels.len(), 2);
    }

    #[test]
    fn parse_gateway_playlist_skips_non_gateway_lines() {
        let config = config(3, 3);
        let playlist = "\
#EXTM3U
#EXTINF:-1,Noise
http://example.com/video.mp4
http://gateway:8080/out/token/stream/01928374-5678-7000-8000-000000000001.ts
";
        let channels = parse_gateway_playlist(playlist, &config);
        assert_eq!(channels.len(), 1);
    }

    #[test]
    fn default_durations_match_acceptance_contract() {
        assert_eq!(DEFAULT_SECONDS, 120);
        assert_eq!(DEFAULT_SOAK_SECONDS, 30 * 60);
        assert_eq!(DEFAULT_CHANNELS, 3);
        assert_eq!(DEFAULT_PROVIDER_CAP, 3);
    }

    #[test]
    fn collect_ts_stream_urls_keeps_only_ts_lines() {
        let playlist = "\
#EXTM3U
http://provider/live/1.ts
http://provider/live/2.mp4
http://provider/live/3.ts
";
        let urls = collect_ts_stream_urls(playlist);
        assert_eq!(urls.len(), 2);
        assert!(urls.iter().all(|url| {
            std::path::Path::new(url)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
        }));
    }

    #[test]
    fn assert_scenario_metrics_accepts_a_clean_three_channel_run() {
        let config = config(3, 3);
        let clean = metrics(3, 3, 3, 0);
        assert!(assert_scenario_metrics(&clean, &config, "three viewers", "warmup").is_ok());
        assert!(assert_scenario_metrics(&clean, &config, "three viewers", "soak").is_ok());
    }

    #[test]
    fn assert_scenario_metrics_rejects_max_connections_below_cap() {
        let config = config(3, 3);
        let mismatched = metrics(3, 3, 2, 0);
        assert!(assert_scenario_metrics(&mismatched, &config, "three viewers", "warmup").is_err());
    }

    #[test]
    fn assert_scenario_metrics_rejects_high_water_above_cap() {
        let config = config(3, 3);
        let oversubscribed = metrics(3, 4, 3, 0);
        assert!(assert_scenario_metrics(&oversubscribed, &config, "six viewers", "soak").is_err());
    }

    #[test]
    fn assert_scenario_metrics_rejects_high_water_below_channel_count() {
        let config = config(3, 3);
        let under_opened = metrics(3, 2, 3, 0);
        assert!(assert_scenario_metrics(&under_opened, &config, "six viewers", "warmup").is_err());
    }

    #[test]
    fn assert_scenario_metrics_rejects_rejected_connections() {
        let config = config(3, 3);
        let rejected = metrics(3, 3, 3, 1);
        assert!(assert_scenario_metrics(&rejected, &config, "six viewers", "soak").is_err());
    }

    #[test]
    fn assert_scenario_metrics_rejects_active_streams_below_channels() {
        let config = config(3, 3);
        let dropped = metrics(2, 3, 3, 0);
        assert!(assert_scenario_metrics(&dropped, &config, "three viewers", "soak").is_err());
    }

    #[test]
    fn assert_scenario_metrics_accepts_two_channel_scale_when_configured() {
        let config = config(2, 2);
        let clean = metrics(2, 2, 2, 0);
        assert!(assert_scenario_metrics(&clean, &config, "two viewers", "warmup").is_ok());
    }

    #[test]
    fn env_bool_parses_case_insensitive_true_values() {
        for value in ["1", "true", "TRUE", "True", "yes", "YES", "on", "ON"] {
            assert!(
                parse_env_bool(Some(value), "TEST", false).unwrap(),
                "value {value:?} must parse as true"
            );
        }
        for value in ["0", "false", "FALSE", "False", "no", "NO", "off", "OFF"] {
            assert!(
                !parse_env_bool(Some(value), "TEST", true).unwrap(),
                "value {value:?} must parse as false"
            );
        }
        assert!(!parse_env_bool(None, "TEST", false).unwrap());
        assert!(parse_env_bool(None, "TEST", true).unwrap());
    }

    #[test]
    fn env_bool_rejects_unknown_values() {
        assert!(parse_env_bool(Some("maybe"), "TEST", false).is_err());
    }

    #[test]
    fn env_bool_trims_whitespace_before_parsing() {
        assert!(parse_env_bool(Some("  true  "), "TEST", false).unwrap());
        assert!(!parse_env_bool(Some("  0  "), "TEST", true).unwrap());
    }

    #[test]
    fn env_usize_rejects_non_numeric_values() {
        assert!(parse_env_usize(Some("not-a-number"), "TEST", 3).is_err());
    }

    #[test]
    fn env_usize_uses_default_when_unset() {
        assert_eq!(parse_env_usize(None, "TEST", 7).unwrap(), 7);
    }

    #[test]
    fn env_u64_rejects_non_numeric_values() {
        assert!(parse_env_u64(Some("abc"), "TEST", 7).is_err());
    }

    #[test]
    fn env_u64_uses_default_when_unset() {
        assert_eq!(parse_env_u64(None, "TEST", 9).unwrap(), 9);
    }

    #[test]
    fn gateway_config_debug_redacts_tokens() {
        let config = config(3, 3);
        let debug_output = format!("{config:?}");
        assert!(
            debug_output.contains("<redacted>"),
            "debug output must redact secrets"
        );
        // The raw token values must not appear in debug output. The field
        // names are acceptable because they do not reveal the secret.
        assert!(
            !debug_output.contains("\"token\""),
            "debug output must not expose the raw output token value"
        );
        assert!(
            !debug_output.contains("\"bootstrap\""),
            "debug output must not expose the raw bootstrap token value"
        );
        assert!(debug_output.contains("channels"));
        assert!(debug_output.contains("provider_cap"));
    }
}
