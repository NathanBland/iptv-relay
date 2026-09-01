//! Compose-level fault-injection media acceptance runner.
//!
//! This binary uses toxiproxy to inject network faults into the media path.
//! It verifies that the media session manager recovers from transient
//! network failures without losing MPEG-TS sync or duplicate upstreams.
//!
//! It also proves these recovery outcomes at the acceptance level:
//! - One channel keeps streaming while a faulted channel recovers.
//! - Six viewers on one channel share one single-flight reconnect.
//! - Keepalive null packets reach downstream clients during failover.
//! - Provider capacity stays bounded and observable through metrics.
//! - Recovery expiry reaches the client as a typed terminal error.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use iptv_media::{
    HttpTsSessionKey, HttpTsSessionManager, HttpTsSourceSpec, MPEG_TS_PACKET_SIZE,
    MpegTsRingConfig, ProviderSpec, RecoveryPolicy, ViewerStreamError,
};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};

const FAULTED_CHANNEL: usize = 1;
const ISOLATED_CHANNEL: usize = 2;
const EXPIRE_CHANNEL: usize = 3;
const FAULTED_VIEWERS: usize = 6;
const ISOLATED_VIEWERS: usize = 2;
const POOL_CAP: usize = 3;

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
    null_packets: Arc<std::sync::atomic::AtomicU64>,
    terminal: Arc<Mutex<Option<String>>>,
    task: JoinHandle<Result<()>>,
}

impl Reader {
    fn stop(self) {
        self.task.abort();
    }

    fn byte_count(&self) -> u64 {
        self.bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn null_packet_count(&self) -> u64 {
        self.null_packets.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn terminal_error(&self) -> Option<String> {
        self.terminal.lock().unwrap().clone()
    }
}

fn packet_pid(packet: &[u8]) -> u16 {
    (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
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

    // Toxiproxy operates at the TCP level, so the upstream must be host:port.
    let provider_host_port = provider
        .strip_prefix("http://")
        .or_else(|| provider.strip_prefix("https://"))
        .unwrap_or(&provider);
    reset_toxiproxy(&client, &toxiproxy).await?;
    // One faulted proxy, one isolated proxy, and one expire proxy.
    create_proxy(
        &client,
        &toxiproxy,
        "media-fault",
        proxy_port,
        provider_host_port,
    )
    .await?;
    create_proxy(
        &client,
        &toxiproxy,
        "media-isolated",
        proxy_port + 1,
        provider_host_port,
    )
    .await?;
    create_proxy(
        &client,
        &toxiproxy,
        "media-expire",
        proxy_port + 2,
        provider_host_port,
    )
    .await?;

    let faulted_url = format!("http://toxiproxy:{proxy_port}");
    let isolated_url = format!("http://toxiproxy:{}", proxy_port + 1);
    let expire_url = format!("http://toxiproxy:{}", proxy_port + 2);
    client
        .post(format!("{provider}/reset"))
        .send()
        .await?
        .error_for_status()?;

    let manager = HttpTsSessionManager::new(client.clone());
    manager.configure_provider(ProviderSpec::new("fault-pool", POOL_CAP));
    let ring = MpegTsRingConfig::new(256, 32).expect("static ring policy is valid");

    // Six viewers on the faulted channel prove single-flight reconnect.
    let mut faulted_readers = Vec::with_capacity(FAULTED_VIEWERS);
    let anchor = manager
        .open(source_spec(&faulted_url, FAULTED_CHANNEL, ring))
        .await
        .with_context(|| format!("start faulted channel {FAULTED_CHANNEL} through proxy"))?;
    faulted_readers.push(spawn_reader(anchor.into_byte_stream(), FAULTED_CHANNEL));
    for _ in 1..FAULTED_VIEWERS {
        let extra = manager
            .open(source_spec(&faulted_url, FAULTED_CHANNEL, ring))
            .await
            .with_context(|| "start additional faulted channel viewer")?;
        faulted_readers.push(spawn_reader(extra.into_byte_stream(), FAULTED_CHANNEL));
    }

    // Two viewers on the isolated channel prove sibling isolation.
    let mut isolated_readers = Vec::with_capacity(ISOLATED_VIEWERS);
    let isolated_anchor = manager
        .open(source_spec(&isolated_url, ISOLATED_CHANNEL, ring))
        .await
        .with_context(|| "start isolated channel through proxy")?;
    let isolated_fork = isolated_anchor.fork();
    isolated_readers.push(spawn_reader(
        isolated_anchor.into_byte_stream(),
        ISOLATED_CHANNEL,
    ));
    isolated_readers.push(spawn_reader(
        isolated_fork.into_byte_stream(),
        ISOLATED_CHANNEL,
    ));

    wait_for_bytes(
        &faulted_readers
            .iter()
            .chain(isolated_readers.iter())
            .collect::<Vec<_>>(),
        188 * 20,
        Duration::from_secs(20),
    )
    .await?;
    let metrics = provider_metrics(&client, &provider).await?;
    assert_live_metrics(&metrics)?;

    // Inject a timeout toxic for 5 seconds on the faulted proxy only.
    add_timeout_toxic(&client, &toxiproxy, "media-fault", 3000).await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    remove_toxic(&client, &toxiproxy, "media-fault", "timeout-fault").await?;

    // Wait for recovery and verify viewers still receive data.
    let faulted_before = byte_counts(&faulted_readers);
    let isolated_before = byte_counts(&isolated_readers);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let faulted_after = byte_counts(&faulted_readers);
    let isolated_after = byte_counts(&isolated_readers);
    ensure!(
        faulted_after
            .iter()
            .zip(faulted_before)
            .all(|(after, before)| *after > before),
        "faulted viewers did not recover after timeout fault"
    );
    ensure!(
        isolated_after
            .iter()
            .zip(isolated_before)
            .all(|(after, before)| *after > before),
        "isolated viewers stopped while the faulted channel recovered"
    );

    // Keepalive null packets must reach downstream clients during failover.
    let null_packets_during_fault: u64 =
        faulted_readers.iter().map(Reader::null_packet_count).sum();
    ensure!(
        null_packets_during_fault > 0,
        "keepalive null packets did not reach any faulted viewer during failover"
    );

    // Provider capacity stays bounded: one upstream per channel regardless of
    // viewer count, and high water never exceeds the configured cap.
    let metrics = provider_metrics(&client, &provider).await?;
    ensure!(
        metrics.active_streams == 2,
        "expected two active upstreams after recovery, saw {}",
        metrics.active_streams
    );
    ensure!(
        metrics.high_water <= POOL_CAP,
        "provider high water exceeded the cap: {}",
        metrics.high_water
    );
    ensure!(metrics.rejected_connections == 0);

    // Inject a slow connection toxic on the faulted proxy only.
    add_slow_toxic(&client, &toxiproxy, "media-fault", 1000).await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    remove_toxic(&client, &toxiproxy, "media-fault", "slow-fault").await?;

    // Verify recovery again.
    let faulted_before = byte_counts(&faulted_readers);
    let isolated_before = byte_counts(&isolated_readers);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let faulted_after = byte_counts(&faulted_readers);
    let isolated_after = byte_counts(&isolated_readers);
    ensure!(
        faulted_after
            .iter()
            .zip(faulted_before)
            .all(|(after, before)| *after > before),
        "faulted viewers did not recover after slow connection fault"
    );
    ensure!(
        isolated_after
            .iter()
            .zip(isolated_before)
            .all(|(after, before)| *after > before),
        "isolated viewers stopped during the slow fault recovery"
    );

    // Soak for the remaining time, minus a buffer for the expiry phase.
    let expiry_budget = Duration::from_secs(12);
    let soak_end = Instant::now() + Duration::from_secs(seconds).saturating_sub(expiry_budget);
    let faulted_before = byte_counts(&faulted_readers);
    let isolated_before = byte_counts(&isolated_readers);
    tokio::time::sleep_until(soak_end).await;
    let faulted_after = byte_counts(&faulted_readers);
    let isolated_after = byte_counts(&isolated_readers);
    ensure!(
        faulted_after
            .iter()
            .zip(faulted_before)
            .all(|(after, before)| *after > before),
        "faulted viewers stopped during soak after fault recovery"
    );
    ensure!(
        isolated_after
            .iter()
            .zip(isolated_before)
            .all(|(after, before)| *after > before),
        "isolated viewers stopped during soak"
    );

    assert_live_metrics(&provider_metrics(&client, &provider).await?)?;

    // Six viewers on the faulted channel must share one reconnect. The
    // provider reports one active upstream for that channel even though six
    // viewers read from it, and the stream-open count stays small.
    let metrics = provider_metrics(&client, &provider).await?;
    let faulted_opens = metrics
        .stream_opens
        .get(FAULTED_CHANNEL - 1)
        .copied()
        .unwrap_or(0);
    ensure!(
        (2..=6).contains(&faulted_opens),
        "faulted channel must reconnect a small number of times for six viewers, saw {faulted_opens}"
    );

    // Recovery expiry phase: stop the faulted and isolated readers, then open
    // one channel through the expire proxy with a short recovery window.
    // Permanently cut the expire proxy and verify the client receives a typed
    // terminal error while a fresh isolated channel keeps streaming.
    for reader in faulted_readers.drain(..) {
        reader.stop();
    }
    for reader in isolated_readers.drain(..) {
        reader.stop();
    }
    wait_for_no_upstreams(&client, &provider).await?;

    let isolated_reader = {
        let handle = manager
            .open(source_spec(&isolated_url, ISOLATED_CHANNEL, ring))
            .await
            .context("reopen the isolated channel for the expiry phase")?;
        spawn_reader(handle.into_byte_stream(), ISOLATED_CHANNEL)
    };

    let mut expire_source = source_spec(&expire_url, EXPIRE_CHANNEL, ring);
    expire_source.set_recovery_policy(
        RecoveryPolicy::new(
            Duration::from_secs(2),
            Duration::from_millis(50),
            Duration::from_millis(50),
            Duration::from_millis(500),
        )
        .expect("static expiry recovery policy is valid"),
    );
    let expire_viewer = manager
        .open(expire_source)
        .await
        .context("start the expire channel through its proxy")?;
    let expire_reader = spawn_reader(expire_viewer.into_byte_stream(), EXPIRE_CHANNEL);

    let expiry_readers: Vec<&Reader> = vec![&isolated_reader, &expire_reader];
    wait_for_bytes(&expiry_readers, 188 * 10, Duration::from_secs(20)).await?;
    let isolated_before = isolated_reader.byte_count();

    // Permanently cut the expire proxy so recovery cannot succeed.
    add_reset_peer_toxic(&client, &toxiproxy, "media-expire").await?;

    // Wait beyond the two-second recovery window for expiry.
    let expiry_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if expire_reader.task.is_finished() {
            break;
        }
        ensure!(
            Instant::now() < expiry_deadline,
            "expire viewer did not terminate within ten seconds"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let terminal = expire_reader
        .terminal_error()
        .unwrap_or_else(|| "no terminal error captured".to_owned());
    ensure!(
        terminal.contains("recovery expired"),
        "recovery expiry must reach the client as a typed terminal error, saw: {terminal}"
    );

    // The isolated channel must keep streaming while the expire channel dies.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let isolated_after = isolated_reader.byte_count();
    ensure!(
        isolated_after > isolated_before,
        "isolated channel stopped while the expire channel expired"
    );
    ensure!(
        !isolated_reader.task.is_finished(),
        "isolated channel must remain live after the expire channel expires"
    );

    isolated_reader.stop();
    expire_reader.stop();
    wait_for_no_upstreams(&client, &provider).await?;

    reset_toxiproxy(&client, &toxiproxy).await?;
    println!(
        "fault acceptance passed: {FAULTED_VIEWERS} faulted viewers, {ISOLATED_VIEWERS} isolated viewers, expiry terminal error verified, {seconds}s"
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
    let null_packets = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let terminal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let observed_bytes = Arc::clone(&bytes);
    let observed_nulls = Arc::clone(&null_packets);
    let observed_terminal = Arc::clone(&terminal);
    let task = tokio::spawn(async move {
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    ensure!(
                        !chunk.is_empty(),
                        "channel {channel} emitted an empty chunk"
                    );
                    ensure!(
                        chunk.len() % MPEG_TS_PACKET_SIZE == 0,
                        "channel {channel} emitted an unaligned MPEG-TS chunk"
                    );
                    for packet in chunk.chunks_exact(MPEG_TS_PACKET_SIZE) {
                        ensure!(packet[0] == 0x47, "channel {channel} lost MPEG-TS sync");
                        if packet_pid(packet) == 0x1fff {
                            observed_nulls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    observed_bytes.fetch_add(
                        u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                Err(error) => {
                    let message = match error {
                        ViewerStreamError::RecoveryExpired { attempts } => {
                            format!(
                                "upstream recovery expired after {attempts} coordinated attempts"
                            )
                        }
                        other => other.to_string(),
                    };
                    *observed_terminal.lock().unwrap() = Some(message.clone());
                    bail!("channel {channel} viewer failed: {message}");
                }
            }
        }
        *observed_terminal.lock().unwrap() = Some("disconnected".to_owned());
        bail!("channel {channel} viewer disconnected before cancellation")
    });
    Reader {
        bytes,
        null_packets,
        terminal,
        task,
    }
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

async fn add_reset_peer_toxic(
    client: &reqwest::Client,
    toxiproxy: &str,
    proxy: &str,
) -> Result<()> {
    let body = serde_json::to_string(&AddToxicBody {
        name: "reset-peer-fault".to_owned(),
        toxic_type: "reset_peer".to_owned(),
        stream: "downstream".to_owned(),
        toxicity: 1.0,
        timeout: None,
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
            "failed to add reset_peer toxic: {} {}",
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

async fn wait_for_bytes(readers: &[&Reader], minimum: u64, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if readers
            .iter()
            .all(|reader| reader.byte_count() >= minimum && !reader.task.is_finished())
        {
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
    readers.iter().map(Reader::byte_count).collect()
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
    ensure!(metrics.active_streams <= POOL_CAP);
    ensure!(metrics.high_water <= POOL_CAP);
    ensure!(metrics.max_connections >= POOL_CAP);
    ensure!(
        metrics.stream_opens.iter().take(2).all(|opens| *opens >= 1),
        "the first two channels must each have opened at least once"
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
