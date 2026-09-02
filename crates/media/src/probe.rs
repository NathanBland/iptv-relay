//! Scheduled low-priority stream health probe.
//!
//! The probe acquires a low-priority provider slot, fetches a short window of
//! the upstream MPEG-TS stream, and validates PAT, PMT, audio, video, and
//! sustained packet flow. Failures are typed and redacted: no URL, header, or
//! credential data leaves the probe.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{Client, header::HeaderMap};
use thiserror::Error;

use crate::{
    AcquireError, MPEG_TS_PACKET_SIZE, MpegTsPacketizer, PacketizerError, ProviderSlotBroker,
    SlotLease,
    psi::{ElementaryStreamInfo, PatPmtTracker, parse_pmt_elementary_streams},
};

/// Configuration for one stream health probe.
#[derive(Clone, Debug)]
pub struct StreamProbeSpec {
    /// The decrypted upstream URL. The probe never includes this in outputs.
    pub url: Arc<str>,
    /// HTTP headers to send with the upstream request.
    pub headers: HeaderMap,
    /// The low-priority provider pool identifier.
    pub pool_id: Arc<str>,
    /// The shared session key for slot accounting.
    pub session_key: Arc<str>,
    /// The low-priority pool capacity. A probe never shares a slot with live
    /// viewers because it uses a dedicated `pool_id` suffix.
    pub max_connections: usize,
    /// Timeout for the upstream to return response headers.
    pub startup_timeout: Duration,
    /// Maximum wall-clock duration for the probe window.
    pub max_duration: Duration,
    /// Minimum transport packets required before the probe declares the stream
    /// alive.
    pub min_packets: usize,
}

/// Quality data extracted from a successful probe.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamProbeQuality {
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

/// A typed, redacted failure reason. No URL, header, or credential data is
/// included.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StreamProbeFailure {
    #[error("provider pool is at capacity")]
    CapacityFull,
    #[error("upstream did not respond before the startup timeout")]
    StartupTimeout,
    #[error("upstream HTTP request failed")]
    Http,
    #[error("upstream returned an unsuccessful HTTP status")]
    HttpStatus,
    #[error("no PAT section was observed")]
    NoPat,
    #[error("no PMT section was observed")]
    NoPmt,
    #[error("no video elementary stream was declared")]
    NoVideo,
    #[error("no audio elementary stream was declared")]
    NoAudio,
    #[error("packet flow stopped before the minimum packet count")]
    InsufficientPackets { seen: usize, required: usize },
    #[error("transport stream data was malformed")]
    MalformedTransport,
}

/// The outcome of one stream health probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamProbeOutcome {
    /// The stream passed all validation checks.
    Alive {
        quality: StreamProbeQuality,
        packets_seen: usize,
        duration_ms: u64,
    },
    /// The stream failed one validation check.
    Dead(StreamProbeFailure),
    /// The probe did not run because the low-priority pool was full.
    Skipped,
}

/// Maps an MPEG-TS stream type to a codec name.
fn codec_name_for_stream_type(stream_type: u8) -> Option<&'static str> {
    match stream_type {
        0x01 => Some("mpeg1-video"),
        0x02 => Some("mpeg2-video"),
        0x10 => Some("mpeg4-video"),
        0x1b => Some("h264"),
        0x24 => Some("h265"),
        0x03 => Some("mpeg1-audio"),
        0x04 => Some("mp2"),
        0x0f => Some("aac"),
        0x11 => Some("aac-latm"),
        0x80 | 0x81 => Some("ac3"),
        0x82 => Some("dts"),
        0x83 | 0x84 | 0x87 => Some("eac3"),
        _ => None,
    }
}

/// Classifies an elementary stream list into video and audio codec names.
fn classify_streams(streams: &[ElementaryStreamInfo]) -> StreamProbeQuality {
    let mut video_codec = None;
    let mut audio_codec = None;
    for entry in streams {
        let Some(name) = codec_name_for_stream_type(entry.stream_type) else {
            continue;
        };
        if name.contains("video") || matches!(entry.stream_type, 0x01 | 0x02 | 0x10 | 0x1b | 0x24) {
            if video_codec.is_none() {
                video_codec = Some(name.to_owned());
            }
        } else if audio_codec.is_none() {
            audio_codec = Some(name.to_owned());
        }
    }
    StreamProbeQuality {
        video_codec,
        audio_codec,
    }
}

/// Analyzes a sequence of MPEG-TS body chunks without any network access.
///
/// The function feeds each chunk through a packetizer, validates PAT and PMT
/// through [`PatPmtTracker`], classifies the declared elementary streams, and
/// confirms that at least `min_packets` transport packets arrive. The probe
/// stops as soon as it sees the minimum packet count after PAT/PMT validation,
/// or when `max_duration` elapses.
///
/// This is the pure analysis core. [`StreamProbe::run`] wraps it with slot
/// acquisition and HTTP fetch.
#[allow(clippy::similar_names)]
pub async fn analyze_transport_stream<S, E>(
    chunks: S,
    min_packets: usize,
    max_duration: Duration,
) -> StreamProbeOutcome
where
    S: Stream<Item = Result<Bytes, E>>,
{
    let deadline = tokio::time::Instant::now() + max_duration;
    let mut packetizer = MpegTsPacketizer::new();
    let mut tracker = PatPmtTracker::default();
    let mut packets_seen = 0_usize;
    let mut pat_seen = false;
    let mut pmt_seen = false;
    let mut quality = StreamProbeQuality::default();
    let start = tokio::time::Instant::now();

    let mut chunks = std::pin::pin!(chunks);
    loop {
        let next = tokio::time::timeout_at(deadline, chunks.next()).await;
        match next {
            Ok(Some(Ok(bytes))) => {
                let completed = match packetizer.push(&bytes) {
                    Ok(completed) => completed,
                    Err(PacketizerError::InvalidSyncByte { .. }) => {
                        return StreamProbeOutcome::Dead(StreamProbeFailure::MalformedTransport);
                    }
                    Err(PacketizerError::IncompleteTail { .. }) => {
                        // A truncated tail at the end of the probe window is
                        // acceptable. Continue to the packet-count check.
                        Bytes::new()
                    }
                };
                if completed.is_empty() {
                    continue;
                }
                for packet in completed.chunks_exact(MPEG_TS_PACKET_SIZE) {
                    packets_seen += 1;
                    let sequence = u64::try_from(packets_seen).unwrap_or(u64::MAX);
                    if let Some(boundary) = tracker.observe(packet, sequence) {
                        pat_seen = true;
                        // observe returns the PAT boundary once the referenced
                        // PMT is also validated.
                        let _ = boundary;
                        pmt_seen = true;
                        if let Some((section, program)) = tracker.completed_pmt()
                            && let Some(streams) = parse_pmt_elementary_streams(section, program)
                        {
                            quality = classify_streams(&streams);
                        }
                    } else if tracker.candidate_start().is_some() {
                        // A PAT section was assembled and is waiting for its
                        // referenced PMT. Track PAT separately from PMT so a
                        // stream with PAT but no PMT reports NoPmt, not NoPat.
                        pat_seen = true;
                    }
                }
                if pmt_seen
                    && quality.video_codec.is_some()
                    && quality.audio_codec.is_some()
                    && packets_seen >= min_packets
                {
                    return StreamProbeOutcome::Alive {
                        quality,
                        packets_seen,
                        duration_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    };
                }
            }
            Ok(Some(Err(_))) => {
                return StreamProbeOutcome::Dead(StreamProbeFailure::Http);
            }
            Ok(None) | Err(_) => break,
        }
    }

    if !pat_seen {
        return StreamProbeOutcome::Dead(StreamProbeFailure::NoPat);
    }
    if !pmt_seen {
        return StreamProbeOutcome::Dead(StreamProbeFailure::NoPmt);
    }
    if quality.video_codec.is_none() {
        return StreamProbeOutcome::Dead(StreamProbeFailure::NoVideo);
    }
    if quality.audio_codec.is_none() {
        return StreamProbeOutcome::Dead(StreamProbeFailure::NoAudio);
    }
    if packets_seen < min_packets {
        return StreamProbeOutcome::Dead(StreamProbeFailure::InsufficientPackets {
            seen: packets_seen,
            required: min_packets,
        });
    }
    StreamProbeOutcome::Alive {
        quality,
        packets_seen,
        duration_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    }
}

/// Runs one stream health probe end to end.
#[derive(Debug)]
pub struct StreamProbe {
    broker: ProviderSlotBroker,
    client: Client,
}

impl StreamProbe {
    /// Creates a probe bound to one low-priority provider pool.
    pub fn new(broker: ProviderSlotBroker, client: Client) -> Self {
        Self { broker, client }
    }

    /// Acquires a low-priority slot, fetches the upstream, and analyzes the
    /// transport stream. Returns [`StreamProbeOutcome::Skipped`] when the
    /// low-priority pool is full.
    ///
    /// # Errors
    ///
    /// This method never returns an error. All failures are reported through
    /// [`StreamProbeOutcome::Dead`].
    pub async fn run(&self, spec: &StreamProbeSpec) -> StreamProbeOutcome {
        let _lease: SlotLease = match self.broker.try_acquire(Arc::clone(&spec.session_key)) {
            Ok(lease) => lease,
            Err(AcquireError::AtCapacity { .. }) => return StreamProbeOutcome::Skipped,
            Err(AcquireError::LeaseIdExhausted) => {
                return StreamProbeOutcome::Dead(StreamProbeFailure::CapacityFull);
            }
        };

        let response = match tokio::time::timeout(
            spec.startup_timeout,
            self.client
                .get(spec.url.as_ref())
                .headers(spec.headers.clone())
                .send(),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return StreamProbeOutcome::Dead(StreamProbeFailure::Http),
            Err(_) => return StreamProbeOutcome::Dead(StreamProbeFailure::StartupTimeout),
        };
        if !response.status().is_success() {
            return StreamProbeOutcome::Dead(StreamProbeFailure::HttpStatus);
        }

        let chunks = response.bytes_stream();
        analyze_transport_stream(chunks, spec.min_packets, spec.max_duration).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::stream;

    use super::*;
    use crate::psi::test_support::{
        TEST_PMT_PID, TEST_VIDEO_PID, pat_packet, pmt_section_with_audio, psi_packet,
    };

    const TEST_AUDIO_PID: u16 = 0x102;

    fn ts_chunk_with_pat_pmt_and_payload(packet_count: usize) -> Bytes {
        let pat = pat_packet();
        let pmt = psi_packet(
            TEST_PMT_PID,
            &pmt_section_with_audio(TEST_VIDEO_PID, TEST_AUDIO_PID),
            0,
            0,
        );
        let mut bytes = Vec::with_capacity(packet_count * MPEG_TS_PACKET_SIZE);
        bytes.extend_from_slice(&pat);
        bytes.extend_from_slice(&pmt);
        // Add payload packets on the video and audio PIDs to exercise sustained
        // packet flow. Use payload-only packets (adaptation control 0b10).
        for index in 0..packet_count.saturating_sub(2) {
            let pid = if index.is_multiple_of(2) {
                TEST_VIDEO_PID
            } else {
                TEST_AUDIO_PID
            };
            let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
            packet[0] = 0x47;
            packet[1] = u8::try_from((pid >> 8) & 0x1f).unwrap();
            packet[2] = u8::try_from(pid & 0xff).unwrap();
            packet[3] = 0x10 | u8::try_from(index & 0x0f).unwrap();
            bytes.extend_from_slice(&packet);
        }
        Bytes::from(bytes)
    }

    #[tokio::test]
    async fn analyze_declares_alive_when_pat_pmt_audio_video_and_flow_are_present() {
        let chunk = ts_chunk_with_pat_pmt_and_payload(8);
        let chunks = stream::iter(vec![Ok::<Bytes, std::io::Error>(chunk)]);
        let outcome = analyze_transport_stream(chunks, 4, Duration::from_secs(1)).await;
        let StreamProbeOutcome::Alive {
            quality,
            packets_seen,
            ..
        } = outcome
        else {
            panic!("expected Alive, got {outcome:?}");
        };
        assert_eq!(quality.video_codec.as_deref(), Some("h264"));
        assert_eq!(quality.audio_codec.as_deref(), Some("mp2"));
        assert!(packets_seen >= 4);
    }

    #[tokio::test]
    async fn analyze_reports_no_pat_when_the_stream_is_empty() {
        let chunks = stream::iter(Vec::<Result<Bytes, std::io::Error>>::new());
        let outcome = analyze_transport_stream(chunks, 1, Duration::from_millis(50)).await;
        assert_eq!(outcome, StreamProbeOutcome::Dead(StreamProbeFailure::NoPat));
    }

    #[tokio::test]
    async fn analyze_reports_insufficient_packets_when_flow_stops_early() {
        // Only PAT + PMT, no payload packets.
        let pat = pat_packet();
        let pmt = psi_packet(
            TEST_PMT_PID,
            &pmt_section_with_audio(TEST_VIDEO_PID, TEST_AUDIO_PID),
            0,
            0,
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&pat);
        bytes.extend_from_slice(&pmt);
        let chunks = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(bytes))]);
        let outcome = analyze_transport_stream(chunks, 10, Duration::from_millis(50)).await;
        assert_eq!(
            outcome,
            StreamProbeOutcome::Dead(StreamProbeFailure::InsufficientPackets {
                seen: 2,
                required: 10
            })
        );
    }

    #[tokio::test]
    async fn analyze_reports_malformed_transport_on_bad_sync_byte() {
        let chunks = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(vec![
            0x00;
            MPEG_TS_PACKET_SIZE
        ]))]);
        let outcome = analyze_transport_stream(chunks, 1, Duration::from_millis(50)).await;
        assert_eq!(
            outcome,
            StreamProbeOutcome::Dead(StreamProbeFailure::MalformedTransport)
        );
    }

    #[tokio::test]
    async fn analyze_reports_no_pmt_when_pat_is_present_but_pmt_is_missing() {
        // Only a PAT packet, no PMT. The probe must report NoPmt, not NoPat,
        // because the PAT section was observed.
        let pat = pat_packet();
        let chunks = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(pat.to_vec()))]);
        let outcome = analyze_transport_stream(chunks, 1, Duration::from_millis(50)).await;
        assert_eq!(outcome, StreamProbeOutcome::Dead(StreamProbeFailure::NoPmt));
    }

    #[tokio::test]
    async fn run_returns_skipped_when_the_low_priority_pool_is_full() {
        let broker = ProviderSlotBroker::new("probe-pool", 1);
        // Fill the single slot with a different session key.
        let _occupier = broker
            .try_acquire("other-probe")
            .expect("occupy the probe slot");
        let probe = StreamProbe::new(broker, Client::new());
        let spec = StreamProbeSpec {
            url: "http://127.0.0.1:1/stream.ts".into(),
            headers: HeaderMap::new(),
            pool_id: "probe-pool".into(),
            session_key: "this-probe".into(),
            max_connections: 1,
            startup_timeout: Duration::from_millis(10),
            max_duration: Duration::from_millis(10),
            min_packets: 1,
        };
        assert_eq!(probe.run(&spec).await, StreamProbeOutcome::Skipped);
    }
}
