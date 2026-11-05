//! Passive capture of stream codec and size data from live viewer sessions.
//!
//! The capture reads from the shared ring buffer of a session that a viewer
//! already opened. It opens no provider connection. A channel is sampled at
//! most one time in each 24 hours for each API process. The database guard in
//! `record_passive_stream_info` enforces the same limit across processes.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use iptv_media::{StreamMetadata, ViewerHandle, sample_cursor_metadata};
use iptv_persistence::{CatalogRepository, StreamHealthUpdate};
use tracing::{debug, warn};
use uuid::Uuid;

/// The minimum interval between two stored updates for one stream.
pub(crate) const PASSIVE_CAPTURE_INTERVAL: Duration = Duration::from_hours(24);
/// The maximum time one sample waits for stream data.
const PASSIVE_SAMPLE_WINDOW: Duration = Duration::from_secs(10);
/// The maximum number of channels that the local throttle remembers.
const THROTTLE_CAPACITY: usize = 4_096;

static LAST_SAMPLE: LazyLock<Mutex<HashMap<Uuid, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns true and records the time when the channel is due for a sample.
fn claim_sample_slot(channel_id: Uuid, now: Instant) -> bool {
    let Ok(mut map) = LAST_SAMPLE.lock() else {
        return false;
    };
    if let Some(last) = map.get(&channel_id)
        && now.duration_since(*last) < PASSIVE_CAPTURE_INTERVAL
    {
        return false;
    }
    if map.len() >= THROTTLE_CAPACITY {
        map.retain(|_, last| now.duration_since(*last) < PASSIVE_CAPTURE_INTERVAL);
    }
    if map.len() >= THROTTLE_CAPACITY {
        return false;
    }
    map.insert(channel_id, now);
    true
}

/// Starts a background sample of a viewer session when the channel is due.
pub(crate) fn spawn_passive_capture(
    catalog: Option<&CatalogRepository>,
    channel_id: Uuid,
    viewer: &ViewerHandle,
) {
    let Some(catalog) = catalog.cloned() else {
        return;
    };
    if !claim_sample_slot(channel_id, Instant::now()) {
        return;
    }
    let cursor = viewer.passive_cursor();
    tokio::spawn(async move {
        let Some(metadata) = sample_cursor_metadata(cursor, PASSIVE_SAMPLE_WINDOW).await else {
            debug!(%channel_id, "passive capture found no stream data");
            return;
        };
        let primary = match catalog.channel_playback_plan(channel_id).await {
            Ok(Some(plan)) => plan.candidates.first().map(|row| row.provider_stream_id),
            Ok(None) => None,
            Err(error) => {
                warn!(%channel_id, error = %error, "passive capture could not load the playback plan");
                return;
            }
        };
        let Some(provider_stream_id) = primary else {
            return;
        };
        let update = passive_update(provider_stream_id, &metadata);
        let seconds = i64::try_from(PASSIVE_CAPTURE_INTERVAL.as_secs()).unwrap_or(i64::MAX);
        match catalog.record_passive_stream_info(&update, seconds).await {
            Ok(stored) => debug!(%channel_id, stored, "passive capture finished"),
            Err(error) => {
                warn!(%channel_id, error = %error, "passive capture could not store stream data");
            }
        }
    });
}

/// Builds the stored values from measured metadata.
fn passive_update(provider_stream_id: Uuid, metadata: &StreamMetadata) -> StreamHealthUpdate {
    let width = metadata
        .video_width
        .and_then(|value| i32::try_from(value).ok());
    let height = metadata
        .video_height
        .and_then(|value| i32::try_from(value).ok());
    StreamHealthUpdate {
        provider_stream_id,
        // The passive path never changes the health status. The store method
        // ignores this field.
        status: "unknown".to_owned(),
        error: None,
        video_codec: metadata.video_codec.clone(),
        video_resolution: height.map(|value| format!("{value}p")),
        video_width: width,
        video_height: height,
        video_fps: metadata.video_fps,
        audio_codec: metadata.audio_codec.clone(),
        audio_channels: metadata.audio_channels.map(i32::from),
        audio_sample_rate: metadata
            .audio_sample_rate
            .and_then(|value| i32::try_from(value).ok()),
        bitrate_kbps: None,
        check_duration_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_allows_one_sample_in_each_interval() {
        let channel = Uuid::new_v4();
        let start = Instant::now();
        assert!(claim_sample_slot(channel, start));
        assert!(!claim_sample_slot(channel, start + Duration::from_mins(1)));
        assert!(claim_sample_slot(
            channel,
            start + PASSIVE_CAPTURE_INTERVAL + Duration::from_secs(1)
        ));
    }

    #[test]
    fn update_keeps_measured_values_and_derives_resolution_label() {
        let metadata = StreamMetadata {
            video_codec: Some("h264".to_owned()),
            video_width: Some(1280),
            video_height: Some(720),
            video_fps: Some(29.97),
            audio_codec: Some("aac".to_owned()),
            audio_channels: Some(2),
            audio_sample_rate: Some(48_000),
        };
        let update = passive_update(Uuid::nil(), &metadata);
        assert_eq!(update.video_resolution.as_deref(), Some("720p"));
        assert_eq!(update.video_width, Some(1280));
        assert_eq!(update.audio_channels, Some(2));
        assert_eq!(update.audio_sample_rate, Some(48_000));
    }
}
