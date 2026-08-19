//! Compose-level deterministic media acceptance runner.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use iptv_media::{
    AcquireError, HttpTsSessionKey, HttpTsSessionManager, HttpTsSourceSpec, MPEG_TS_PACKET_SIZE,
    MpegTsRingConfig, ProviderSpec, SessionStartError,
};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};

const CHANNELS: usize = 3;
const VIEWERS_PER_CHANNEL: usize = 2;

#[derive(Debug, Deserialize)]
struct ProviderMetrics {
    active_streams: usize,
    high_water: usize,
    max_connections: usize,
    stream_opens: [u64; CHANNELS],
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
async fn main() -> Result<()> {
    let provider = std::env::var("FAKE_PROVIDER_URL")
        .unwrap_or_else(|_| "http://fake-provider:8090".to_owned())
        .trim_end_matches('/')
        .to_owned();
    let seconds = std::env::var("MEDIA_ACCEPTANCE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120_u64);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    wait_for_provider(&client, &provider).await?;
    client
        .post(format!("{provider}/reset"))
        .send()
        .await?
        .error_for_status()?;

    let manager = HttpTsSessionManager::new(client.clone());
    manager.configure_provider(ProviderSpec::new("acceptance-pool", CHANNELS));
    let ring = MpegTsRingConfig::new(256, 32).expect("static ring policy is valid");
    let mut readers = Vec::with_capacity(CHANNELS * VIEWERS_PER_CHANNEL);
    for channel in 1..=CHANNELS {
        let first = manager
            .open(source_spec(&provider, channel, ring))
            .await
            .with_context(|| format!("start channel {channel}"))?;
        let second = first.fork();
        readers.push(spawn_reader(first.into_byte_stream(), channel));
        readers.push(spawn_reader(second.into_byte_stream(), channel));
    }

    wait_for_bytes(&readers, 188 * 20, Duration::from_secs(20)).await?;
    let metrics = provider_metrics(&client, &provider).await?;
    assert_live_metrics(&metrics)?;
    let snapshot = manager
        .provider_snapshot("acceptance-pool")
        .context("provider broker disappeared")?;
    ensure!(snapshot.active_sessions == CHANNELS);
    ensure!(snapshot.high_watermark == CHANNELS);

    let fourth = manager.open(source_spec(&provider, 4, ring)).await;
    ensure!(matches!(
        fourth,
        Err(SessionStartError::Provider(AcquireError::AtCapacity {
            capacity: CHANNELS,
            ..
        }))
    ));
    ensure!(provider_metrics(&client, &provider).await?.high_water == CHANNELS);

    let end = Instant::now() + Duration::from_secs(seconds);
    let before = byte_counts(&readers);
    tokio::time::sleep_until(end).await;
    let after = byte_counts(&readers);
    ensure!(
        after
            .iter()
            .zip(before)
            .all(|(after, before)| *after > before),
        "one or more viewers stopped receiving media during the soak"
    );
    assert_live_metrics(&provider_metrics(&client, &provider).await?)?;

    // Disconnect one viewer per channel. The other three viewers must retain
    // all three shared upstream sessions.
    let mut survivors = Vec::with_capacity(CHANNELS);
    for (index, reader) in readers.into_iter().enumerate() {
        if index % VIEWERS_PER_CHANNEL == 0 {
            reader.stop();
        } else {
            survivors.push(reader);
        }
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    ensure!(provider_metrics(&client, &provider).await?.active_streams == CHANNELS);
    ensure!(survivors.iter().all(|reader| !reader.task.is_finished()));

    for reader in survivors {
        reader.stop();
    }
    wait_for_no_upstreams(&client, &provider).await?;
    let final_metrics = provider_metrics(&client, &provider).await?;
    ensure!(final_metrics.stream_opens == [1, 1, 1]);
    ensure!(final_metrics.rejected_connections == 0);
    ensure!(
        manager
            .provider_snapshot("acceptance-pool")
            .context("provider broker disappeared")?
            .active_sessions
            == 0
    );
    println!(
        "media acceptance passed: {CHANNELS} upstreams, {} viewers, {seconds}s",
        CHANNELS * VIEWERS_PER_CHANNEL
    );
    Ok(())
}

fn source_spec(provider: &str, channel: usize, ring: MpegTsRingConfig) -> HttpTsSourceSpec {
    HttpTsSourceSpec::new(
        HttpTsSessionKey::new("acceptance-pool", format!("channel-{channel}"), 1),
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
        .bytes()
        .await?;
    Ok(serde_json::from_slice(&body)?)
}

fn assert_live_metrics(metrics: &ProviderMetrics) -> Result<()> {
    ensure!(metrics.active_streams == CHANNELS);
    ensure!(metrics.high_water == CHANNELS);
    ensure!(metrics.max_connections == CHANNELS);
    ensure!(metrics.stream_opens == [1, 1, 1]);
    ensure!(metrics.rejected_connections == 0);
    Ok(())
}

async fn wait_for_no_upstreams(client: &reqwest::Client, provider: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if provider_metrics(client, provider).await?.active_streams == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "final viewers did not close upstreams within two seconds"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_specs_share_generation_per_channel_but_not_channel_identity() {
        let ring = MpegTsRingConfig::new(8, 2).unwrap();
        let first = source_spec("http://provider.test", 1, ring);
        let duplicate = source_spec("http://provider.test", 1, ring);
        let second = source_spec("http://provider.test", 2, ring);
        assert_eq!(first.key(), duplicate.key());
        assert_ne!(first.key(), second.key());
    }

    #[test]
    fn live_metric_contract_rejects_oversubscription_or_reopens() {
        let valid = ProviderMetrics {
            active_streams: 3,
            high_water: 3,
            max_connections: 3,
            stream_opens: [1, 1, 1],
            rejected_connections: 0,
        };
        assert!(assert_live_metrics(&valid).is_ok());
        assert!(
            assert_live_metrics(&ProviderMetrics {
                high_water: 4,
                ..valid
            })
            .is_err()
        );
    }
}
