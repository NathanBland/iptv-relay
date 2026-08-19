//! Live provider media acceptance runner.
//!
//! This binary downloads a real M3U playlist from the configured provider,
//! selects the first active stream, and verifies that it receives valid
//! MPEG-TS data for a configurable duration.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use iptv_media::MPEG_TS_PACKET_SIZE;

#[tokio::main]
async fn main() -> Result<()> {
    let m3u_url = std::env::var("IPTV_TEST_M3U_URL")
        .context("IPTV_TEST_M3U_URL must be set to a live M3U playlist URL")?;
    let seconds = std::env::var("LIVE_ACCEPTANCE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30_u64);
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

    println!(
        "live acceptance passed: {packet_count} packets, {total_bytes} bytes, {seconds}s"
    );
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

    ensure!(
        response.status().is_success(),
        "the live stream returned status {}",
        response.status()
    );

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
