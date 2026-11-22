//! Stores the bitrate, burst and frame rate measured by live viewer sessions.
//!
//! The ring of each session measures its stream from the PCR and the video
//! timestamps (see `iptv_media::meter`). This module reads those values in the
//! background while a viewer watches, and stores them for the provider stream.
//! The next session of the channel then sizes its ring from the measurement.
//! The capture opens no provider connection and does not keep the session or
//! its ring alive.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use iptv_media::{RingRateReader, StreamRates, ViewerHandle};
use iptv_persistence::{CatalogRepository, StreamMeasurement};
use tracing::{debug, warn};
use uuid::Uuid;

/// The minimum interval between two captures for one stream.
const CAPTURE_INTERVAL: Duration = Duration::from_hours(1);
/// Media time that a measurement must cover before it is stored.
const MIN_MEASURED: Duration = Duration::from_mins(1);
/// How often the capture reads the ring.
const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// The capture stops reading after this time and stores what it has.
const MAX_CAPTURE: Duration = Duration::from_mins(10);
/// The maximum number of streams that the throttle remembers.
const THROTTLE_CAPACITY: usize = 4_096;

static LAST_CAPTURE: LazyLock<Mutex<HashMap<Uuid, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns true and records the time when the stream is due for a capture.
fn claim_capture_slot(provider_stream_id: Uuid, now: Instant) -> bool {
    let Ok(mut map) = LAST_CAPTURE.lock() else {
        return false;
    };
    if let Some(last) = map.get(&provider_stream_id)
        && now.duration_since(*last) < CAPTURE_INTERVAL
    {
        return false;
    }
    if map.len() >= THROTTLE_CAPACITY {
        map.retain(|_, last| now.duration_since(*last) < CAPTURE_INTERVAL);
    }
    if map.len() >= THROTTLE_CAPACITY {
        return false;
    }
    map.insert(provider_stream_id, now);
    true
}

/// Gives back a slot that a capture claimed at `claimed_at` and could not use.
///
/// A short probe session often claims the slot first. Its ring closes when
/// the probe disconnects, so its capture measures nothing. The release lets
/// the next viewer of the stream capture without a wait for the interval.
fn release_capture_slot(provider_stream_id: Uuid, claimed_at: Instant) {
    let Ok(mut map) = LAST_CAPTURE.lock() else {
        return;
    };
    if map.get(&provider_stream_id) == Some(&claimed_at) {
        map.remove(&provider_stream_id);
    }
}

/// Starts a background capture of the session's measured rates when the
/// stream is due.
pub(crate) fn spawn_rate_capture(
    catalog: Option<&CatalogRepository>,
    provider_stream_id: Option<Uuid>,
    viewer: &ViewerHandle,
) {
    let (Some(catalog), Some(provider_stream_id)) = (catalog.cloned(), provider_stream_id) else {
        return;
    };
    let claimed_at = Instant::now();
    if !claim_capture_slot(provider_stream_id, claimed_at) {
        return;
    }
    let reader = viewer.rate_reader();
    tokio::spawn(async move {
        let Some(rates) = watch_rates(&reader, POLL_INTERVAL, MAX_CAPTURE).await else {
            debug!(%provider_stream_id, "rate capture ended before enough media was measured");
            release_capture_slot(provider_stream_id, claimed_at);
            return;
        };
        let Some(measurement) = measurement(provider_stream_id, &rates) else {
            release_capture_slot(provider_stream_id, claimed_at);
            return;
        };
        match catalog.record_stream_measurement(&measurement).await {
            Ok(stored) => debug!(
                %provider_stream_id,
                stored,
                average_kbps = measurement.average_bitrate_kbps,
                peak_kbps = measurement.peak_bitrate_kbps,
                burst_ms = measurement.burst_ms,
                frame_rate = measurement.frame_rate,
                "rate capture finished"
            ),
            Err(error) => {
                warn!(%provider_stream_id, error = %error, "rate capture could not store the measurement");
            }
        }
    });
}

/// Reads the ring until `max_wait` passes or the measurement stops (the
/// session ended or the upstream reconnected). A ring that closes before it
/// measures enough media ends the capture at the next poll. Returns the newest rates that
/// cover at least [`MIN_MEASURED`] of media.
async fn watch_rates(
    reader: &RingRateReader,
    poll: Duration,
    max_wait: Duration,
) -> Option<StreamRates> {
    let deadline = tokio::time::Instant::now() + max_wait;
    let mut best: Option<StreamRates> = None;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(poll).await;
        match reader.rates() {
            Some(rates) if best.is_none_or(|best| rates.measured >= best.measured) => {
                best = Some(rates);
            }
            Some(_) => break,
            None if best.is_some() || reader.is_gone() => break,
            None => {}
        }
    }
    best.filter(|rates| rates.measured >= MIN_MEASURED)
}

/// Converts measured rates to stored values.
fn measurement(provider_stream_id: Uuid, rates: &StreamRates) -> Option<StreamMeasurement> {
    let kbps = |bits: u64| {
        i32::try_from(bits.div_ceil(1_000))
            .ok()
            .filter(|value| *value > 0)
    };
    Some(StreamMeasurement {
        provider_stream_id,
        average_bitrate_kbps: kbps(rates.average_bits_per_second)?,
        peak_bitrate_kbps: kbps(rates.peak_bits_per_second)?,
        burst_ms: i32::try_from(rates.burst.as_millis()).unwrap_or(i32::MAX),
        frame_rate: rates
            .frame_rate_millihertz
            .map(|millihertz| f64::from(millihertz) / 1_000.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates(measured_seconds: u64) -> StreamRates {
        StreamRates {
            average_bits_per_second: 7_812_400,
            peak_bits_per_second: 9_100_000,
            burst: Duration::from_millis(14_250),
            frame_rate_millihertz: Some(59_940),
            measured: Duration::from_secs(measured_seconds),
        }
    }

    #[test]
    fn throttle_allows_one_capture_in_each_interval() {
        let stream = Uuid::new_v4();
        let start = Instant::now();
        assert!(claim_capture_slot(stream, start));
        assert!(!claim_capture_slot(stream, start + Duration::from_mins(5)));
        assert!(claim_capture_slot(
            stream,
            start + CAPTURE_INTERVAL + Duration::from_secs(1)
        ));
    }

    #[test]
    fn measurement_rounds_up_to_whole_kilobits() {
        let stream = Uuid::new_v4();
        let stored = measurement(stream, &rates(90)).unwrap();
        assert_eq!(stored.average_bitrate_kbps, 7_813);
        assert_eq!(stored.peak_bitrate_kbps, 9_100);
        assert_eq!(stored.burst_ms, 14_250);
        assert_eq!(stored.frame_rate, Some(59.94));
    }

    #[tokio::test(start_paused = true)]
    async fn capture_waits_for_enough_media() {
        let ring = iptv_media::MpegTsRing::new(iptv_media::MpegTsRingConfig::new(16, 0).unwrap());
        let reader = ring.rate_reader();
        // An empty ring never measures anything, so the capture stores nothing.
        assert_eq!(
            watch_rates(&reader, POLL_INTERVAL, Duration::from_mins(1)).await,
            None
        );
        drop(ring);
        assert_eq!(reader.rates(), None);
    }

    #[test]
    fn released_slot_lets_the_next_viewer_capture() {
        let stream = Uuid::new_v4();
        let start = Instant::now();
        assert!(claim_capture_slot(stream, start));
        release_capture_slot(stream, start);
        let next = start + Duration::from_secs(2);
        assert!(claim_capture_slot(stream, next));
    }

    #[test]
    fn stale_release_keeps_a_newer_claim() {
        let stream = Uuid::new_v4();
        let start = Instant::now();
        assert!(claim_capture_slot(stream, start));
        // A release from a different claim time does not free the slot.
        release_capture_slot(stream, start + Duration::from_secs(1));
        assert!(!claim_capture_slot(stream, start + Duration::from_secs(2)));
    }

    #[tokio::test(start_paused = true)]
    async fn capture_of_a_closed_ring_ends_at_the_next_poll() {
        let ring = iptv_media::MpegTsRing::new(iptv_media::MpegTsRingConfig::new(16, 0).unwrap());
        let reader = ring.rate_reader();
        drop(ring);
        assert!(reader.is_gone());
        let started = tokio::time::Instant::now();
        assert_eq!(watch_rates(&reader, POLL_INTERVAL, MAX_CAPTURE).await, None);
        assert_eq!(started.elapsed(), POLL_INTERVAL);
    }
}
