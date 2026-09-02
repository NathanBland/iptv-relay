//! Deterministic Jellyfin-facing output acceptance runner.
//!
//! This runner models the requests that a Jellyfin M3U tuner makes. It imports
//! the token-protected playlist and guide, checks the device contracts, and
//! opens two viewers per imported channel to prove upstream sharing.

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, future::join_all};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};
use uuid::Uuid;

const DEFAULT_CHANNELS: usize = 3;
const DEFAULT_TUNER_COUNT: usize = 3;
const DEFAULT_SECONDS: u64 = 5;
const SOURCE_NAME: &str = "jellyfin-acceptance-provider";
const GUIDE_SOURCE_NAME: &str = "jellyfin-acceptance-guide";

#[derive(Debug)]
struct Config {
    gateway_url: String,
    output_token: String,
    bootstrap_token: String,
    provider_url: String,
    channels: usize,
    tuner_count: usize,
    seconds: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        let channels = env_usize("JELLYFIN_ACCEPTANCE_CHANNELS", DEFAULT_CHANNELS)?;
        let tuner_count = env_usize("IPTV_TUNER_COUNT", DEFAULT_TUNER_COUNT)?;
        ensure!(
            channels > 0,
            "JELLYFIN_ACCEPTANCE_CHANNELS must be greater than zero"
        );
        ensure!(
            channels <= 3,
            "JELLYFIN_ACCEPTANCE_CHANNELS cannot exceed the deterministic provider's three channels"
        );
        ensure!(
            tuner_count > 0,
            "IPTV_TUNER_COUNT must be greater than zero"
        );
        ensure!(
            !env_bool("JELLYFIN_ACCEPTANCE_SSDP", false)?,
            "SSDP is optional and must remain disabled for deterministic acceptance"
        );
        Ok(Self {
            gateway_url: env_required("IPTV_GATEWAY_URL")?
                .trim_end_matches('/')
                .to_owned(),
            output_token: env_required("IPTV_OUTPUT_TOKEN")?,
            bootstrap_token: env_required("IPTV_ADMIN_BOOTSTRAP_TOKEN")?,
            provider_url: env_required("IPTV_TEST_PROVIDER_URL")?
                .trim_end_matches('/')
                .to_owned(),
            channels,
            tuner_count,
            seconds: env_u64("JELLYFIN_ACCEPTANCE_SECONDS", DEFAULT_SECONDS)?,
        })
    }

    fn output_url(&self, suffix: &str) -> String {
        format!("{}/out/{}/{}", self.gateway_url, self.output_token, suffix)
    }

    fn admin_request(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        client
            .request(method, format!("{}{}", self.gateway_url, path))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.bootstrap_token),
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlaylistEntry {
    id: Uuid,
    name: String,
    stream_url: String,
}

#[derive(Debug, Deserialize)]
struct SourceResponse {
    id: String,
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ProviderMetrics {
    active_streams: usize,
    high_water: usize,
    max_connections: usize,
    stream_opens: [u64; 3],
    rejected_connections: u64,
}

#[derive(Debug, Deserialize)]
struct HdhrDiscovery {
    #[serde(rename = "TunerCount")]
    tuner_count: usize,
    #[serde(rename = "BaseUrl", alias = "BaseURL")]
    base_url: String,
    #[serde(rename = "LineupUrl", alias = "LineupURL")]
    lineup_url: String,
}

#[derive(Debug, Deserialize)]
struct HdhrLineupEntry {
    #[serde(rename = "GuideName")]
    guide_name: String,
    #[serde(rename = "Url", alias = "URL")]
    url: String,
}

struct Viewer {
    bytes: Arc<std::sync::atomic::AtomicU64>,
    task: JoinHandle<Result<()>>,
}

impl Viewer {
    fn stop(self) {
        self.task.abort();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_mins(2))
        .build()?;
    let stream_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_hours(24))
        .build()?;

    wait_for_ready(&client, &config).await?;
    ensure_source(&client, &config).await?;
    ensure_guide_source(&client, &config).await?;
    let playlist = wait_for_playlist(&client, &config).await?;
    let imported = import_playlist(&playlist, &config)?;
    let playlist_again =
        download_text(&client, &config.output_url("playlist.m3u"), "M3U playlist").await?;
    ensure!(
        parse_playlist(&playlist_again) == imported,
        "M3U channel identifiers changed between imports"
    );
    let xmltv = wait_for_xmltv(&client, &config, &imported).await?;
    verify_xmltv_import(&xmltv, &imported)?;
    verify_hdhr(&client, &config, &imported).await?;
    verify_shared_sessions(&client, &stream_client, &config, &imported).await?;

    println!(
        "Jellyfin acceptance passed: {} channels, {} tuners, {}s shared-session run; SSDP disabled",
        imported.len(),
        config.tuner_count,
        config.seconds
    );
    Ok(())
}

async fn wait_for_ready(client: &reqwest::Client, config: &Config) -> Result<()> {
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        let gateway_ready = client
            .get(format!("{}/health/ready", config.gateway_url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        let provider_ready = client
            .get(format!("{}/health", config.provider_url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        if gateway_ready && provider_ready {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the gateway and provider did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn ensure_source(client: &reqwest::Client, config: &Config) -> Result<()> {
    let response = config
        .admin_request(client, reqwest::Method::GET, "/api/v1/sources")
        .send()
        .await
        .context("list gateway sources")?
        .error_for_status()
        .context("gateway rejected source list")?;
    let sources: Vec<SourceResponse> = response.json().await.context("parse source list")?;
    let source_id = sources
        .iter()
        .find(|source| source.name == SOURCE_NAME)
        .map(|source| source.id.clone());
    let source_id = if let Some(source_id) = source_id {
        source_id
    } else {
        let response = config
            .admin_request(client, reqwest::Method::POST, "/api/v1/sources")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({
                "name": SOURCE_NAME,
                "kind": "M3U",
                "endpoint": format!("{}/playlist.m3u", config.provider_url),
            }))
            .send()
            .await
            .context("create Jellyfin acceptance source")?
            .error_for_status()
            .context("gateway rejected Jellyfin acceptance source")?;
        response
            .json::<SourceResponse>()
            .await
            .context("parse created source")?
            .id
    };
    let response = config
        .admin_request(
            client,
            reqwest::Method::PATCH,
            &format!("/api/v1/sources/{source_id}"),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({"maxConnections": config.channels}))
        .send()
        .await
        .context("set Jellyfin acceptance source capacity")?;
    ensure!(
        response.status().is_success(),
        "gateway rejected source capacity update: {}",
        response.status()
    );
    let sync = config
        .admin_request(
            client,
            reqwest::Method::POST,
            &format!("/api/v1/sources/{source_id}/sync"),
        )
        .send()
        .await
        .context("trigger Jellyfin acceptance source sync")?;
    ensure!(
        sync.status().is_success() || sync.status().as_u16() == 409,
        "gateway rejected source sync: {}",
        sync.status()
    );
    Ok(())
}

async fn ensure_guide_source(client: &reqwest::Client, config: &Config) -> Result<()> {
    let response = config
        .admin_request(client, reqwest::Method::GET, "/api/v1/sources")
        .send()
        .await
        .context("list gateway sources for XMLTV")?
        .error_for_status()
        .context("gateway rejected XMLTV source list")?;
    let sources: Vec<SourceResponse> = response.json().await.context("parse XMLTV source list")?;
    let source_id = sources
        .iter()
        .find(|source| {
            source.name == GUIDE_SOURCE_NAME && source.kind.eq_ignore_ascii_case("XMLTV")
        })
        .map(|source| source.id.clone());
    let source_id = if let Some(source_id) = source_id {
        source_id
    } else {
        let response = config
            .admin_request(client, reqwest::Method::POST, "/api/v1/sources")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({
                "name": GUIDE_SOURCE_NAME,
                "kind": "XMLTV",
                "endpoint": format!("{}/xmltv.xml", config.provider_url),
                "timezone": "UTC"
            }))
            .send()
            .await
            .context("create Jellyfin acceptance XMLTV source")?
            .error_for_status()
            .context("gateway rejected Jellyfin acceptance XMLTV source")?;
        response
            .json::<SourceResponse>()
            .await
            .context("parse created XMLTV source")?
            .id
    };
    let sync = config
        .admin_request(
            client,
            reqwest::Method::POST,
            &format!("/api/v1/sources/{source_id}/sync"),
        )
        .send()
        .await
        .context("trigger Jellyfin acceptance XMLTV sync")?;
    ensure!(
        sync.status().is_success() || sync.status().as_u16() == 409,
        "gateway rejected XMLTV source sync: {}",
        sync.status()
    );
    Ok(())
}

async fn wait_for_playlist(client: &reqwest::Client, config: &Config) -> Result<String> {
    let deadline = Instant::now() + Duration::from_mins(2);
    loop {
        let playlist =
            download_text(client, &config.output_url("playlist.m3u"), "M3U playlist").await?;
        if parse_playlist(&playlist).len() == config.channels {
            return Ok(playlist);
        }
        ensure!(
            Instant::now() < deadline,
            "the filtered M3U output did not expose exactly {} channels",
            config.channels
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_xmltv(
    client: &reqwest::Client,
    config: &Config,
    imported: &[PlaylistEntry],
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_mins(2);
    loop {
        let xmltv = download_text(client, &config.output_url("xmltv.xml"), "XMLTV guide").await?;
        if verify_xmltv_import(&xmltv, imported).is_ok() {
            return Ok(xmltv);
        }
        ensure!(
            Instant::now() < deadline,
            "the XMLTV output did not include guide data for all imported channels"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn download_text(client: &reqwest::Client, url: &str, label: &str) -> Result<String> {
    client
        .get(url)
        .send()
        .await
        .with_context(|| format!("download {label}"))?
        .error_for_status()
        .with_context(|| format!("{label} returned an error status"))?
        .text()
        .await
        .with_context(|| format!("read {label} body"))
}

fn import_playlist(playlist: &str, config: &Config) -> Result<Vec<PlaylistEntry>> {
    let entries = parse_playlist(playlist);
    ensure!(
        entries.len() == config.channels,
        "the filtered M3U output has {} channels; expected {}",
        entries.len(),
        config.channels
    );
    let ids: HashSet<_> = entries.iter().map(|entry| entry.id).collect();
    ensure!(
        ids.len() == entries.len(),
        "the M3U output contains duplicate channel identifiers"
    );
    for entry in &entries {
        ensure!(
            entry.stream_url == config.output_url(&format!("stream/{}.ts", entry.id)),
            "channel {} does not use the token-protected gateway stream route",
            entry.id
        );
    }
    Ok(entries)
}

fn parse_playlist(playlist: &str) -> Vec<PlaylistEntry> {
    let mut entries = Vec::new();
    let mut pending: Option<(String, Uuid)> = None;
    for line in playlist.lines().map(str::trim) {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let Some(raw_id) = attribute(info, "tvg-id") else {
                pending = None;
                continue;
            };
            let Ok(id) = Uuid::parse_str(&raw_id) else {
                pending = None;
                continue;
            };
            let name = info
                .rsplit(',')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            pending = Some((name, id));
        } else if let Some((name, id)) = pending.take()
            && line.starts_with("http")
        {
            entries.push(PlaylistEntry {
                id,
                name,
                stream_url: line.to_owned(),
            });
        }
    }
    entries
}

async fn verify_hdhr(
    client: &reqwest::Client,
    config: &Config,
    imported: &[PlaylistEntry],
) -> Result<()> {
    let discovery: HdhrDiscovery = client
        .get(config.output_url("hdhr/discover.json"))
        .send()
        .await
        .context("download HDHomeRun discovery")?
        .error_for_status()
        .context("HDHomeRun discovery returned an error status")?
        .json()
        .await
        .context("parse HDHomeRun discovery")?;
    ensure!(
        discovery.tuner_count == config.tuner_count,
        "HDHomeRun reports {} tuners; expected {}",
        discovery.tuner_count,
        config.tuner_count
    );
    ensure!(
        discovery.base_url == config.output_url("hdhr"),
        "HDHomeRun base URL is not stable"
    );
    ensure!(
        discovery.lineup_url == config.output_url("hdhr/lineup.json"),
        "HDHomeRun lineup URL is not stable"
    );

    let lineup: Vec<HdhrLineupEntry> = client
        .get(config.output_url("hdhr/lineup.json"))
        .send()
        .await
        .context("download HDHomeRun lineup")?
        .error_for_status()
        .context("HDHomeRun lineup returned an error status")?
        .json()
        .await
        .context("parse HDHomeRun lineup")?;
    ensure!(
        lineup.len() == imported.len(),
        "HDHomeRun lineup is not filtered to the M3U channel set"
    );
    for (line, entry) in lineup.iter().zip(imported) {
        ensure!(
            line.guide_name == entry.name,
            "HDHomeRun guide name differs from M3U"
        );
        ensure!(
            line.url == entry.stream_url,
            "HDHomeRun stream URL differs from M3U"
        );
    }

    let status = client
        .get(config.output_url("hdhr/lineup_status.json"))
        .send()
        .await
        .context("download HDHomeRun lineup status")?
        .error_for_status()
        .context("HDHomeRun lineup status returned an error status")?
        .json::<serde_json::Value>()
        .await
        .context("parse HDHomeRun lineup status")?;
    ensure!(
        status["ScanInProgress"] == 0,
        "HDHomeRun lineup scan is still in progress"
    );
    let device = download_text(
        client,
        &config.output_url("hdhr/device.xml"),
        "HDHomeRun device description",
    )
    .await?;
    ensure!(
        device.contains("HDHR-IPTV"),
        "HDHomeRun device description is missing the model name"
    );
    Ok(())
}

fn verify_xmltv_import(xmltv: &str, imported: &[PlaylistEntry]) -> Result<()> {
    let now = chrono::Utc::now();
    let mut has_active = false;
    for entry in imported {
        ensure!(
            xmltv.contains(&format!("<channel id=\"{}\"", entry.id)),
            "XMLTV output is missing channel {}",
            entry.id
        );
        let programme = format!("<programme channel=\"{}\"", entry.id);
        ensure!(
            xmltv.contains(&programme),
            "XMLTV output has no programme for {}",
            entry.id
        );
        let offset = xmltv.find(&programme).expect("programme was checked above");
        ensure!(
            xmltv[offset..].contains("<title>"),
            "XMLTV programme for {} has no title",
            entry.id
        );
        let programme_end = xmltv[offset..]
            .find("</programme>")
            .map_or(xmltv.len(), |end| offset + end);
        let programme = &xmltv[offset..programme_end];
        if let (Some(start), Some(stop)) =
            (attribute(programme, "start"), attribute(programme, "stop"))
            && let (Ok(start), Ok(stop)) = (
                chrono::DateTime::parse_from_str(&start, "%Y%m%d%H%M%S %z"),
                chrono::DateTime::parse_from_str(&stop, "%Y%m%d%H%M%S %z"),
            )
            && start.with_timezone(&chrono::Utc) <= now
            && now < stop.with_timezone(&chrono::Utc)
        {
            has_active = true;
        }
    }
    ensure!(
        has_active,
        "XMLTV output has no programme active at the current time"
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn verify_shared_sessions(
    client: &reqwest::Client,
    stream_client: &reqwest::Client,
    config: &Config,
    imported: &[PlaylistEntry],
) -> Result<()> {
    client
        .post(format!("{}/reset", config.provider_url))
        .send()
        .await
        .context("reset fake provider")?
        .error_for_status()
        .context("fake provider could not reset")?;
    let opens = imported.iter().flat_map(|entry| {
        [0, 1].into_iter().map(move |_| {
            let client = stream_client.clone();
            let url = entry.stream_url.clone();
            async move {
                client
                    .get(url)
                    .send()
                    .await
                    .context("open Jellyfin stream")?
                    .error_for_status()
                    .context("Jellyfin stream returned an error status")
            }
        })
    });
    let responses = join_all(opens).await;
    let mut viewers = Vec::with_capacity(responses.len());
    for response in responses {
        let response = response?;
        let mut stream = response.bytes_stream();
        let bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = Arc::clone(&bytes);
        let task = tokio::spawn(async move {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("read Jellyfin stream")?;
                ensure!(!chunk.is_empty(), "Jellyfin stream returned an empty chunk");
                if observed.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                    ensure!(
                        chunk[0] == 0x47,
                        "Jellyfin stream does not start with MPEG-TS"
                    );
                }
                observed.fetch_add(
                    u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            bail!("Jellyfin stream disconnected before cancellation")
        });
        viewers.push(Viewer { bytes, task });
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if viewers
            .iter()
            .all(|viewer| viewer.bytes.load(std::sync::atomic::Ordering::Relaxed) > 188 * 20)
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Jellyfin viewers did not receive MPEG-TS in time"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let metrics = provider_metrics(client, config).await?;
    ensure!(
        metrics.active_streams == imported.len(),
        "{} upstream sessions serve {} imported channels",
        metrics.active_streams,
        imported.len()
    );
    ensure!(
        metrics.high_water == imported.len(),
        "shared-session high water is {}",
        metrics.high_water
    );
    ensure!(
        metrics.max_connections == config.channels,
        "provider capacity is {}",
        metrics.max_connections
    );
    ensure!(
        metrics.rejected_connections == 0,
        "provider rejected {} shared viewers",
        metrics.rejected_connections
    );
    ensure!(
        imported.len() <= metrics.stream_opens.len(),
        "provider metrics expose too few per-channel counters"
    );
    ensure!(
        metrics.stream_opens[..imported.len()]
            .iter()
            .all(|opens| *opens == 1),
        "provider opened more than one upstream session for a channel: {:?}",
        metrics.stream_opens
    );
    tokio::time::sleep(Duration::from_secs(config.seconds)).await;
    for viewer in viewers {
        viewer.stop();
    }
    wait_for_no_upstreams(client, config).await?;
    Ok(())
}

async fn provider_metrics(client: &reqwest::Client, config: &Config) -> Result<ProviderMetrics> {
    client
        .get(format!("{}/metrics.json", config.provider_url))
        .send()
        .await
        .context("read fake provider metrics")?
        .error_for_status()
        .context("fake provider metrics returned an error status")?
        .json()
        .await
        .context("parse fake provider metrics")
}

async fn wait_for_no_upstreams(client: &reqwest::Client, config: &Config) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if provider_metrics(client, config).await?.active_streams == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "shared upstream sessions did not close"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn attribute(value: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let start = value.find(&marker)? + marker.len();
    Some(value[start..].split('"').next()?.to_owned())
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set"))
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    std::env::var(name).ok().map_or(Ok(default), |value| {
        value
            .parse()
            .with_context(|| format!("{name} must be a non-negative integer"))
    })
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    std::env::var(name).ok().map_or(Ok(default), |value| {
        value
            .parse()
            .with_context(|| format!("{name} must be a non-negative integer"))
    })
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name).ok().as_deref().map(str::trim) {
        None => Ok(default),
        Some(value)
            if ["1", "true", "yes", "on"].contains(&value.to_ascii_lowercase().as_str()) =>
        {
            Ok(true)
        }
        Some(value)
            if ["0", "false", "no", "off"].contains(&value.to_ascii_lowercase().as_str()) =>
        {
            Ok(false)
        }
        Some(value) => bail!("{name} must be a boolean, got {value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_parser_preserves_stable_uuid_and_name() {
        let id = "01928374-5678-7000-8000-123456789abc";
        let playlist = format!(
            "#EXTM3U\n#EXTINF:-1 tvg-id=\"{id}\" tvg-name=\"One\",One\nhttp://gateway/out/token/stream/{id}.ts\n"
        );
        let entries = parse_playlist(&playlist);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, Uuid::parse_str(id).unwrap());
        assert_eq!(entries[0].name, "One");
    }

    #[test]
    fn bool_parser_accepts_disabled_values() {
        for value in ["0", "false", "NO", "off"] {
            assert!(!parse_bool(Some(value), true).unwrap());
        }
        assert!(!parse_bool(None, false).unwrap());
    }

    fn parse_bool(value: Option<&str>, default: bool) -> Result<bool> {
        match value {
            None => Ok(default),
            Some(value)
                if ["0", "false", "no", "off"].contains(&value.to_ascii_lowercase().as_str()) =>
            {
                Ok(false)
            }
            Some(value)
                if ["1", "true", "yes", "on"].contains(&value.to_ascii_lowercase().as_str()) =>
            {
                Ok(true)
            }
            Some(value) => bail!("invalid boolean {value}"),
        }
    }
}
