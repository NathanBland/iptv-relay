//! Deterministic provider used by connection-sharing acceptance tests.

use std::{
    env,
    fmt::Write as _,
    net::SocketAddr,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result};
use async_stream::stream;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use serde::Serialize;
use tokio::{io::AsyncReadExt, net::TcpListener, process::Command, signal};

const CHANNEL_COUNT: usize = 3;

#[derive(Debug)]
struct Counters {
    active: AtomicUsize,
    high_water: AtomicUsize,
    opens: [AtomicU64; CHANNEL_COUNT],
    rejected: AtomicU64,
    max_connections: usize,
}

impl Counters {
    fn new(max_connections: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            opens: std::array::from_fn(|_| AtomicU64::new(0)),
            rejected: AtomicU64::new(0),
            max_connections,
        }
    }

    fn reserve(self: &Arc<Self>, channel: usize) -> Option<ConnectionGuard> {
        loop {
            let active = self.active.load(Ordering::SeqCst);
            if active >= self.max_connections {
                self.rejected.fetch_add(1, Ordering::SeqCst);
                return None;
            }
            if self
                .active
                .compare_exchange(active, active + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.high_water.fetch_max(active + 1, Ordering::SeqCst);
                self.opens[channel].fetch_add(1, Ordering::SeqCst);
                return Some(ConnectionGuard {
                    counters: Arc::clone(self),
                });
            }
        }
    }
}

#[derive(Debug)]
struct ConnectionGuard {
    counters: Arc<Counters>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counters.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
struct AppState {
    counters: Arc<Counters>,
    base_url: Arc<str>,
}

#[derive(Debug, Serialize)]
struct Metrics {
    active_streams: usize,
    high_water: usize,
    max_connections: usize,
    stream_opens: [u64; CHANNEL_COUNT],
    rejected_connections: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let bind: SocketAddr = env::var("FAKE_PROVIDER_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_owned())
        .parse()
        .context("parse FAKE_PROVIDER_BIND")?;
    let max_connections = env::var("FAKE_PROVIDER_MAX_CONNECTIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(CHANNEL_COUNT);
    let state = AppState {
        counters: Arc::new(Counters::new(max_connections)),
        base_url: env::var("FAKE_PROVIDER_BASE_URL")
            .unwrap_or_else(|_| format!("http://{bind}"))
            .trim_end_matches('/')
            .into(),
    };
    let app = Router::new()
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .route("/playlist.m3u", get(playlist))
        .route("/xmltv.xml", get(xmltv))
        .route("/stream/{*channel_path}", get(stream))
        .route("/metrics.json", get(metrics))
        .route("/reset", post(reset))
        .with_state(state);
    let listener = TcpListener::bind(bind).await?;
    println!("fake provider listening on {bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}

async fn playlist(State(state): State<AppState>) -> Response {
    let mut body = format!("#EXTM3U x-tvg-url=\"{}/xmltv.xml\"\n", state.base_url);
    for channel in 1..=CHANNEL_COUNT {
        write!(
            &mut body,
            "#EXTINF:-1 tvg-id=\"fake.{channel}\" tvg-name=\"Test Channel {channel}\" group-title=\"Acceptance\",Test Channel {channel}\n{}/stream/{channel}.ts\n",
            state.base_url
        )
        .expect("writing to a String cannot fail");
    }
    typed_body("application/vnd.apple.mpegurl", body)
}

async fn xmltv() -> Response {
    let mut body = String::from(
        "<?xml version=\"1.0\"?><tv generator-info-name=\"IPTV Gateway Test Provider\">",
    );
    for channel in 1..=CHANNEL_COUNT {
        write!(
            &mut body,
            "<channel id=\"fake.{channel}\"><display-name>Test Channel {channel}</display-name></channel>"
        )
        .expect("writing to a String cannot fail");
        write!(
            &mut body,
            "<programme channel=\"fake.{channel}\" start=\"20260101000000 +0000\" stop=\"20300101000000 +0000\"><title>Continuous Test Pattern {channel}</title></programme>"
        )
        .expect("writing to a String cannot fail");
    }
    body.push_str("</tv>");
    typed_body("application/xml", body)
}

async fn stream(State(state): State<AppState>, Path(channel_path): Path<String>) -> Response {
    let Some(channel) = channel_path
        .strip_suffix(".ts")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !(1..=CHANNEL_COUNT).contains(&channel) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(guard) = state.counters.reserve(channel - 1) else {
        let mut response = StatusCode::SERVICE_UNAVAILABLE.into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
        return response;
    };

    let frequency = 300 + channel * 220;
    let color = match channel {
        1 => "red",
        2 => "green",
        _ => "blue",
    };
    let video_source = format!("color=c={color}:size=640x360:rate=30");
    let audio_source = format!("sine=frequency={frequency}:sample_rate=48000");
    let Ok(mut child) = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            &video_source,
            "-f",
            "lavfi",
            "-i",
            &audio_source,
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "mpeg2video",
            "-b:v",
            "1200k",
            "-g",
            "30",
            "-c:a",
            "mp2",
            "-b:a",
            "128k",
            "-mpegts_flags",
            "+resend_headers",
            "-f",
            "mpegts",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some(mut stdout) = child.stdout.take() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let body_stream = stream! {
        let _guard = guard;
        let _child = child;
        let mut buffer = vec![0_u8; 32 * 1024];
        loop {
            match stdout.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    yield Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buffer[..read]));
                }
                Err(error) => {
                    yield Err::<Bytes, std::io::Error>(error);
                    break;
                }
            }
        }
    };
    let mut response = Response::new(Body::from_stream(body_stream));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    response
}

async fn metrics(State(state): State<AppState>) -> Json<Metrics> {
    Json(Metrics {
        active_streams: state.counters.active.load(Ordering::SeqCst),
        high_water: state.counters.high_water.load(Ordering::SeqCst),
        max_connections: state.counters.max_connections,
        stream_opens: std::array::from_fn(|index| {
            state.counters.opens[index].load(Ordering::SeqCst)
        }),
        rejected_connections: state.counters.rejected.load(Ordering::SeqCst),
    })
}

async fn reset(State(state): State<AppState>) -> Response {
    if state.counters.active.load(Ordering::SeqCst) != 0 {
        return (
            StatusCode::CONFLICT,
            "cannot reset while streams are active",
        )
            .into_response();
    }
    state.counters.high_water.store(0, Ordering::SeqCst);
    state.counters.rejected.store(0, Ordering::SeqCst);
    for count in &state.counters.opens {
        count.store(0, Ordering::SeqCst);
    }
    StatusCode::NO_CONTENT.into_response()
}

fn typed_body(content_type: &'static str, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_cap_never_oversubscribes() {
        let counters = Arc::new(Counters::new(2));
        let first = counters.reserve(0).unwrap();
        let second = counters.reserve(1).unwrap();
        assert!(counters.reserve(2).is_none());
        assert_eq!(counters.active.load(Ordering::SeqCst), 2);
        assert_eq!(counters.high_water.load(Ordering::SeqCst), 2);
        drop(first);
        assert!(counters.reserve(2).is_some());
        drop(second);
    }

    #[test]
    fn final_guard_releases_connection() {
        let counters = Arc::new(Counters::new(1));
        let guard = counters.reserve(0).unwrap();
        assert_eq!(counters.active.load(Ordering::SeqCst), 1);
        drop(guard);
        assert_eq!(counters.active.load(Ordering::SeqCst), 0);
    }
}
