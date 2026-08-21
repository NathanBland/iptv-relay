//! Compose-level fault-injection media acceptance runner.
//!
//! This binary uses toxiproxy to inject network faults into the media path.
//! It verifies that the media session manager recovers from transient
//! network failures without losing MPEG-TS sync or duplicate upstreams.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use iptv_media::{
    HttpTsSessionKey, HttpTsSessionManager, HttpTsSourceSpec, MPEG_TS_PACKET_SIZE,
    MpegTsRingConfig, ProviderSpec,
};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};

const CHANNELS: usize = 2;
const VIEWERS_PER_CHANNEL: usize = 2;

#[derive(Debug, Deserialize)]
struct ProviderMetrics {
    active_streams: usize,
    high_water: usize,
    max_connections: usize,
    stream_opens: Vec<u64>,
    rejected_connections: u64,
}

struct Reader {
    bytes: Arc<std::sync::atomic::AtomicU64>,
    task: JoinHandle<Result<()>>,
}

impl Reader {
    fn stop(self) {
        self.task.abort();
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let provider = std::env::var("FAKE_PROVIDER_URL")
        .unwrap_or_else(|_| "http://fake-provider:8090".to_owned())
        .trim_end_matches('/')
        .to_owned();
    let toxiproxy = std::env::var("TOXIPROXY_URL")
        .unwrap_or_else(|_| "http://toxiproxy:8474".to_owned())
        .trim_end_matches('/')
        .to_owned();
    let proxy_port: u16 = std::env::var("TOXIPROXY_PROXY_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8666);
    let seconds = std::env::var("FAULT_ACCEPTANCE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60_u64);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()?;

    wait_for_provider(&client, &provider).await?;
    wait_for_toxiproxy(&client, &toxiproxy).await?;

    // Create a toxiproxy that forwards to the fake provider.
    // Toxiproxy operates at the TCP level, so the upstream must be host:port, not a URL.
    let provider_host_port = provider
        .strip_prefix("http://")
        .or_else(|| provider.strip_prefix("https://"))
        .unwrap_or(&provider);
    reset_toxiproxy(&client, &toxiproxy).await?;
    create_proxy(
        &client,
        &toxiproxy,
        "media-fault",
        proxy_port,
        provider_host_port,
    )
    .await?;

    let proxy_url = format!("http://toxiproxy:{proxy_port}");
    client
        .post(format!("{provider}/reset"))
        .send()
        .await?
        .error_for_status()?;

    let manager = HttpTsSessionManager::new(client.clone());
    manager.configure_provider(ProviderSpec::new("fault-pool", CHANNELS));
    let ring = MpegTsRingConfig::new(256, 32).expect("static ring policy is valid");
    let mut readers = Vec::with_capacity(CHANNELS * VIEWERS_PER_CHANNEL);
    for channel in 1..=CHANNELS {
        let first = manager
            .open(source_spec(&proxy_url, channel, ring))
            .await
            .with_context(|| format!("start channel {channel} through proxy"))?;
        let second = first.fork();
        readers.push(spawn_reader(first.into_byte_stream(), channel));
        readers.push(spawn_reader(second.into_byte_stream(), channel));
    }

    wait_for_bytes(&readers, 188 * 20, Duration::from_secs(20)).await?;
    let metrics = provider_metrics(&client, &provider).await?;
    assert_live_metrics(&metrics)?;

    // Inject a timeout toxic for 5 seconds.
    add_timeout_toxic(&client, &toxiproxy, "media-fault", 3000).await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    remove_toxic(&client, &toxiproxy, "media-fault", "timeout-fault").await?;

    // Wait for recovery and verify viewers still receive data.
    let before = byte_counts(&readers);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let after = byte_counts(&readers);
    ensure!(
        after
            .iter()
            .zip(before)
            .all(|(after, before)| *after > before),
        "viewers did not recover after timeout fault"
    );

    // Inject a slow connection toxic.
    add_slow_toxic(&client, &toxiproxy, "media-fault", 1000).await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    remove_toxic(&client, &toxiproxy, "media-fault", "slow-fault").await?;

    // Verify recovery again.
    let before = byte_counts(&readers);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let after = byte_counts(&readers);
    ensure!(
        after
            .iter()
            .zip(before)
            .all(|(after, before)| *after > before),
        "viewers did not recover after slow connection fault"
    );

    // Soak for the remaining time.
    let end = Instant::now() + Duration::from_secs(seconds);
    let before = byte_counts(&readers);
    tokio::time::sleep_until(end).await;
    let after = byte_counts(&readers);
    ensure!(
        after
            .iter()
            .zip(before)
            .all(|(after, before)| *after > before),
        "viewers stopped during soak after fault recovery"
    );

    assert_live_metrics(&provider_metrics(&client, &provider).await?)?;

    for reader in readers {
        reader.stop();
    }
    wait_for_no_upstreams(&client, &provider).await?;

    reset_toxiproxy(&client, &toxiproxy).await?;
    println!(
        "fault acceptance passed: {CHANNELS} upstreams, {} viewers, {seconds}s",
        CHANNELS * VIEWERS_PER_CHANNEL
    );
    Ok(())
}

fn source_spec(provider: &str, channel: usize, ring: MpegTsRingConfig) -> HttpTsSourceSpec {
    HttpTsSourceSpec::new(
        HttpTsSessionKey::new("fault-pool", format!("channel-{channel}"), 1),
        format!("{provider}/stream/{channel}.ts"),
        ring,
    )
}

fn spawn_reader(stream: iptv_media::ViewerByteStream, channel: usize) -> Reader {
    let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = Arc::clone(&bytes);
    let task = tokio::spawn(async move {
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("channel {channel} viewer failed"))?;
            ensure!(
                !chunk.is_empty(),
                "channel {channel} emitted an empty chunk"
            );
            ensure!(
                chunk.len() % MPEG_TS_PACKET_SIZE == 0,
                "channel {channel} emitted an unaligned MPEG-TS chunk"
            );
            ensure!(
                chunk
                    .chunks_exact(MPEG_TS_PACKET_SIZE)
                    .all(|packet| packet[0] == 0x47),
                "channel {channel} lost MPEG-TS sync"
            );
            observed.fetch_add(
                u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        bail!("channel {channel} viewer disconnected before cancellation")
    });
    Reader { bytes, task }
}

async fn wait_for_provider(client: &reqwest::Client, provider: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if client
            .get(format!("{provider}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "fake provider did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_toxiproxy(client: &reqwest::Client, toxiproxy: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if client
            .get(format!("{toxiproxy}/version"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "toxiproxy did not become ready");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn reset_toxiproxy(client: &reqwest::Client, toxiproxy: &str) -> Result<()> {
    client
        .post(format!("{toxiproxy}/reset"))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

#[derive(serde::Serialize)]
struct CreateProxyBody {
    name: String,
    listen: String,
    upstream: String,
    enabled: bool,
}

async fn create_proxy(
    client: &reqwest::Client,
    toxiproxy: &str,
    name: &str,
    port: u16,
    upstream: &str,
) -> Result<()> {
    let body = serde_json::to_string(&CreateProxyBody {
        name: name.to_owned(),
        listen: format!("0.0.0.0:{port}"),
        upstream: upstream.to_owned(),
        enabled: true,
    })?;
    let response = client
        .post(format!("{toxiproxy}/proxies"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?;
    // 201 or 409 (already exists) are acceptable.
    if !response.status().is_success() && response.status().as_u16() != 409 {
        bail!(
            "failed to create toxiproxy: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct AddToxicBody {
    name: String,
    #[serde(rename = "type")]
    toxic_type: String,
    stream: String,
    toxicity: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delay: Option<u64>,
}

async fn add_timeout_toxic(
    client: &reqwest::Client,
    toxiproxy: &str,
    proxy: &str,
    timeout_ms: u64,
) -> Result<()> {
    let body = serde_json::to_string(&AddToxicBody {
        name: "timeout-fault".to_owned(),
        toxic_type: "timeout".to_owned(),
        stream: "downstream".to_owned(),
        toxicity: 1.0,
        timeout: Some(timeout_ms),
        delay: None,
    })?;
    let response = client
        .post(format!("{toxiproxy}/proxies/{proxy}/toxics"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?;
    if !response.status().is_success() && response.status().as_u16() != 409 {
        bail!(
            "failed to add timeout toxic: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

async fn add_slow_toxic(
    client: &reqwest::Client,
    toxiproxy: &str,
    proxy: &str,
    delay_ms: u64,
) -> Result<()> {
    let body = serde_json::to_string(&AddToxicBody {
        name: "slow-fault".to_owned(),
        toxic_type: "latency".to_owned(),
        stream: "downstream".to_owned(),
        toxicity: 1.0,
        timeout: None,
        delay: Some(delay_ms),
    })?;
    let response = client
        .post(format!("{toxiproxy}/proxies/{proxy}/toxics"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?;
    if !response.status().is_success() && response.status().as_u16() != 409 {
        bail!(
            "failed to add slow toxic: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

async fn remove_toxic(
    client: &reqwest::Client,
    toxiproxy: &str,
    proxy: &str,
    toxic: &str,
) -> Result<()> {
    let response = client
        .delete(format!("{toxiproxy}/proxies/{proxy}/toxics/{toxic}"))
        .send()
        .await?;
    if !response.status().is_success() && response.status().as_u16() != 404 {
        bail!(
            "failed to remove toxic: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

async fn wait_for_bytes(readers: &[Reader], minimum: u64, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if readers.iter().all(|reader| {
            reader.bytes.load(std::sync::atomic::Ordering::Relaxed) >= minimum
                && !reader.task.is_finished()
        }) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "viewers did not receive MPEG-TS in time"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn byte_counts(readers: &[Reader]) -> Vec<u64> {
    readers
        .iter()
        .map(|reader| reader.bytes.load(std::sync::atomic::Ordering::Relaxed))
        .collect()
}

async fn provider_metrics(client: &reqwest::Client, provider: &str) -> Result<ProviderMetrics> {
    let body = client
        .get(format!("{provider}/metrics.json"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    serde_json::from_str(&body).with_context(|| format!("parse provider metrics: {body}"))
}

fn assert_live_metrics(metrics: &ProviderMetrics) -> Result<()> {
    ensure!(metrics.active_streams == CHANNELS);
    ensure!(metrics.high_water == CHANNELS);
    ensure!(metrics.max_connections >= CHANNELS);
    ensure!(
        metrics
            .stream_opens
            .iter()
            .take(CHANNELS)
            .all(|opens| *opens >= 1)
    );
    ensure!(metrics.rejected_connections == 0);
    Ok(())
}

async fn wait_for_no_upstreams(client: &reqwest::Client, provider: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if provider_metrics(client, provider).await?.active_streams == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "final viewers did not close upstreams within five seconds"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
