//! Compose-level deterministic media acceptance runner.

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, future::join_all};
use iptv_media::{
    AcquireError, HttpTsSessionKey, HttpTsSessionManager, HttpTsSourceSpec, MPEG_TS_PACKET_SIZE,
    MpegTsRingConfig, ProviderSpec, SessionStartError,
};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};

const CHANNELS: usize = 3;
const DEFAULT_VIEWER_COUNTS: [usize; 2] = [1, 2];

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
    identity_verified: Arc<std::sync::atomic::AtomicBool>,
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
    let viewer_counts = configured_viewer_counts()?;
    for viewers_per_channel in viewer_counts {
        run_scenario(&client, &provider, seconds, viewers_per_channel).await?;
    }
    println!(
        "media acceptance passed: {CHANNELS} channels, viewer counts {:?}, {seconds}s each",
        configured_viewer_counts()?
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_scenario(
    client: &reqwest::Client,
    provider: &str,
    seconds: u64,
    viewers_per_channel: usize,
) -> Result<()> {
    client
        .post(format!("{provider}/reset"))
        .send()
        .await?
        .error_for_status()?;

    let manager = HttpTsSessionManager::new(client.clone());
    manager.configure_provider(ProviderSpec::new("acceptance-pool", CHANNELS));
    let ring = MpegTsRingConfig::new(256, 32).expect("static ring policy is valid");
    let mut readers = Vec::with_capacity(CHANNELS * viewers_per_channel);
    for channel in 1..=CHANNELS {
        let source = source_spec(provider, channel, ring);
        let gate = Arc::new(tokio::sync::Barrier::new(viewers_per_channel));
        let opens = (0..viewers_per_channel).map(|_| {
            let manager = manager.clone();
            let source = source.clone();
            let gate = Arc::clone(&gate);
            async move {
                gate.wait().await;
                manager.open(source).await
            }
        });
        let opened = join_all(opens).await;
        let opened: Vec<_> = opened
            .into_iter()
            .map(|viewer| viewer.with_context(|| format!("start channel {channel}")))
            .collect::<Result<_>>()?;
        let lease_id = opened
            .first()
            .context("viewer race returned no handles")?
            .lease_id();
        ensure!(
            opened.iter().all(|viewer| viewer.lease_id() == lease_id),
            "first-viewer race created duplicate upstream sessions"
        );
        ensure!(
            opened
                .iter()
                .all(|viewer| { viewer.session_snapshot().viewer_count == viewers_per_channel })
        );
        for viewer in opened {
            readers.push(spawn_reader(viewer.into_byte_stream(), channel));
        }
    }

    wait_for_bytes(&readers, 188 * 20, Duration::from_secs(20)).await?;
    assert_live_metrics(&provider_metrics(client, provider).await?)?;
    let snapshot = manager
        .provider_snapshot("acceptance-pool")
        .context("provider broker disappeared")?;
    ensure!(snapshot.active_sessions == CHANNELS);
    ensure!(snapshot.high_watermark == CHANNELS);

    let fourth = manager.open(source_spec(provider, 4, ring)).await;
    ensure!(matches!(
        fourth,
        Err(SessionStartError::Provider(AcquireError::AtCapacity {
            capacity: CHANNELS,
            ..
        }))
    ));
    ensure!(provider_metrics(client, provider).await?.high_water == CHANNELS);

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
    ensure!(
        readers.iter().all(|reader| !reader.task.is_finished()),
        "one or more viewers dropped during the soak"
    );
    assert_live_metrics(&provider_metrics(client, provider).await?)?;
    assert_wrapped_without_recovery(&manager)?;

    let mut survivors = Vec::with_capacity(CHANNELS * viewers_per_channel);
    for (index, reader) in readers.into_iter().enumerate() {
        if index % viewers_per_channel == 0 {
            reader.stop();
        } else {
            survivors.push(reader);
        }
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    if viewers_per_channel > 1 {
        ensure!(provider_metrics(client, provider).await?.active_streams == CHANNELS);
        ensure!(survivors.iter().all(|reader| !reader.task.is_finished()));
    }

    for reader in survivors {
        reader.stop();
    }
    wait_for_no_upstreams(client, provider).await?;
    let final_metrics = provider_metrics(client, provider).await?;
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
        "media scenario passed: {CHANNELS} upstreams, {} viewers, {seconds}s",
        CHANNELS * viewers_per_channel
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
    let identity_verified = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_identity = Arc::clone(&identity_verified);
    let task = tokio::spawn(async move {
        let mut stream = Box::pin(stream);
        let mut continuity = HashMap::<u16, u8>::new();
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
            for packet in chunk.chunks_exact(MPEG_TS_PACKET_SIZE) {
                verify_continuity(packet, channel, &mut continuity)?;
                if let Some(service_id) = pat_service_id(packet) {
                    ensure!(
                        service_id == channel,
                        "channel {channel} received service {service_id}"
                    );
                    observed_identity.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            observed.fetch_add(
                u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        bail!("channel {channel} viewer disconnected before cancellation")
    });
    Reader {
        bytes,
        identity_verified,
        task,
    }
}

fn verify_continuity(
    packet: &[u8],
    channel: usize,
    continuity: &mut HashMap<u16, u8>,
) -> Result<()> {
    let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
    if pid == 0x1fff {
        return Ok(());
    }
    let adaptation_control = (packet[3] >> 4) & 0x03;
    ensure!(
        adaptation_control != 0,
        "channel {channel} emitted an invalid TS header"
    );
    let discontinuity =
        matches!(adaptation_control, 2 | 3) && packet[4] > 0 && packet[5] & 0x80 != 0;
    if discontinuity {
        continuity.remove(&pid);
    }
    if !matches!(adaptation_control, 1 | 3) {
        return Ok(());
    }
    let counter = packet[3] & 0x0f;
    if let Some(previous) = continuity.insert(pid, counter) {
        ensure!(
            counter == (previous + 1) & 0x0f,
            "channel {channel} has a continuity gap on PID {pid}"
        );
    }
    Ok(())
}

fn pat_service_id(packet: &[u8]) -> Option<usize> {
    let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
    if pid != 0 || packet[1] & 0x40 == 0 {
        return None;
    }
    let adaptation_control = (packet[3] >> 4) & 0x03;
    if !matches!(adaptation_control, 1 | 3) {
        return None;
    }
    let payload_offset = if adaptation_control == 3 {
        5_usize.checked_add(usize::from(packet[4]))?
    } else {
        4
    };
    let section_offset = payload_offset
        .checked_add(1)?
        .checked_add(usize::from(*packet.get(payload_offset)?))?;
    let section = packet.get(section_offset..)?;
    if section.len() < 12 || section[0] != 0 {
        return None;
    }
    let section_length = (usize::from(section[1] & 0x0f) << 8) | usize::from(section[2]);
    let section_end = 3_usize.checked_add(section_length)?;
    if section_end > section.len() || section_length < 13 {
        return None;
    }
    section[8..section_end.saturating_sub(4)]
        .chunks_exact(4)
        .map(|program| usize::from(u16::from_be_bytes([program[0], program[1]])))
        .find(|program| *program != 0)
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
                && reader
                    .identity_verified
                    .load(std::sync::atomic::Ordering::Acquire)
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

fn assert_wrapped_without_recovery(manager: &HttpTsSessionManager) -> Result<()> {
    let snapshots = manager.list_snapshots();
    ensure!(snapshots.len() == CHANNELS);
    ensure!(snapshots.iter().all(|snapshot| {
        snapshot.ring.first_sequence > 0
            && snapshot.ring.retained_packets == snapshot.ring.capacity_packets
            && snapshot.ring.closed.is_none()
            && snapshot.ring_wrap_events > 0
            && snapshot.overwritten_packets > 0
            && snapshot.reconnect_attempts == 0
            && snapshot.failover_attempts == 0
            && snapshot.failure_count == 0
            && snapshot.last_failure.is_none()
    }));
    Ok(())
}

fn configured_viewer_counts() -> Result<Vec<usize>> {
    let Some(value) = std::env::var_os("MEDIA_ACCEPTANCE_VIEWERS_PER_CHANNEL") else {
        return Ok(DEFAULT_VIEWER_COUNTS.to_vec());
    };
    let value = value
        .to_str()
        .context("MEDIA_ACCEPTANCE_VIEWERS_PER_CHANNEL is not valid UTF-8")?;
    let viewers = value
        .parse::<usize>()
        .with_context(|| format!("invalid MEDIA_ACCEPTANCE_VIEWERS_PER_CHANNEL value {value:?}"))?;
    ensure!(
        DEFAULT_VIEWER_COUNTS.contains(&viewers),
        "MEDIA_ACCEPTANCE_VIEWERS_PER_CHANNEL must be 1 or 2"
    );
    Ok(vec![viewers])
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

    #[test]
    fn default_scenarios_cover_three_and_six_viewers() {
        assert_eq!(DEFAULT_VIEWER_COUNTS, [1, 2]);
        assert_eq!(
            DEFAULT_VIEWER_COUNTS.map(|viewers| CHANNELS * viewers),
            [3, 6]
        );
    }
}
