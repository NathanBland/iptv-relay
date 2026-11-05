//! Stream slates and the slate-to-live splice.
//!
//! A slate is a short `FFmpeg` render that a client receives while the live
//! stream is not available. The relay shows three slates:
//!
//! - "Stream starting in N seconds" while the live buffer fills.
//! - "All tuners in use, stay tuned for one to open up" while the provider
//!   pool is full.
//! - "Having trouble reaching this channel, retrying" after a provider or
//!   startup error.
//!
//! Each slate agrees with the last known stream profile of the channel, so
//! the probe of a client (for example Jellyfin) gives the same decision for
//! the slate and for the live stream. The [`SpliceMuxer`] joins the slate and
//! the live stream into one MPEG-TS program with fixed PIDs, a new PAT and PMT
//! at each splice, a `discontinuity_indicator`, and rebased PTS and PCR values
//! that always increase.

use std::{
    ffi::OsString,
    fmt,
    future::{Future, pending},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use tokio::time::Instant;
use tracing::{info, warn};

use crate::{
    AcquireError, FfmpegInputAdapter, HttpTsSessionManager, HttpTsSourceSpec, MPEG_TS_PACKET_SIZE,
    MpegTsRingConfig, ProcessInputSession, RingCursor, RingRead, RingReadError, SessionStartError,
    ViewerByteStream, ViewerHandle, ViewerStreamError,
    psi::{PatPmtTracker, mpeg_crc32},
};

/// Output PID of the program map table.
pub const SLATE_PMT_PID: u16 = 0x1000;
/// Output PID of the video elementary stream.
pub const SLATE_VIDEO_PID: u16 = 0x100;
/// Output PID of the audio elementary stream.
pub const SLATE_AUDIO_PID: u16 = 0x101;
/// Output PID of a PCR that has its own PID in the input.
pub const SLATE_PCR_PID: u16 = 0x102;

/// The longest buffered lead that a new viewer waits for before live output.
pub const SLATE_START_LEAD: Duration = Duration::from_secs(5);
/// The time that a request waits for live media before a slate starts.
pub const DEFAULT_SLATE_GRACE: Duration = Duration::from_millis(1_500);
/// The retry interval while the provider pool is full.
pub const CAPACITY_RETRY_INTERVAL: Duration = Duration::from_millis(500);
/// The first retry interval after a provider or startup error.
pub const PROVIDER_RETRY_INITIAL: Duration = Duration::from_secs(1);
/// The longest retry interval after repeated provider or startup errors.
pub const PROVIDER_RETRY_MAX: Duration = Duration::from_secs(30);

/// The smallest countdown on the starting slate when the lead is unknown.
const STARTING_COUNTDOWN_FLOOR: Duration = Duration::from_secs(3);
/// A viewer with less lead than the target starts after this extra time.
const LEAD_WAIT_LIMIT: Duration = Duration::from_secs(10);
/// A viewer with no media after this time counts as a startup error.
const NO_MEDIA_LIMIT: Duration = Duration::from_secs(30);
/// The interval of readiness and retry checks.
const SLATE_TICK: Duration = Duration::from_millis(250);
/// The shortest interval between two slate process starts.
const SLATE_RESTART_INTERVAL: Duration = Duration::from_secs(2);
/// Ring size of one slate process. A slate is about 200 kbit/s.
const SLATE_RING_PACKETS: usize = 22_000;
/// Packets in each slate read.
const SLATE_BATCH_PACKETS: usize = 32;

// ---------------------------------------------------------------------------
// Slate profile
// ---------------------------------------------------------------------------

/// The video codec of a slate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlateVideoCodec {
    H264,
    H265,
    Mpeg2,
}

/// The audio codec of a slate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlateAudioCodec {
    Aac,
    Ac3,
    Eac3,
    Mp2,
}

/// A frame rate as a fraction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl FrameRate {
    const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    fn as_f64(self) -> f64 {
        f64::from(self.numerator) / f64::from(self.denominator)
    }

    /// Frames in one second, rounded up. The slate uses one keyframe each second.
    fn keyframe_interval(self) -> u32 {
        self.numerator.div_ceil(self.denominator).max(1)
    }
}

/// The broadcast frame rates that a slate can use.
const FRAME_RATES: [FrameRate; 8] = [
    FrameRate::new(24_000, 1_001),
    FrameRate::new(24, 1),
    FrameRate::new(25, 1),
    FrameRate::new(30_000, 1_001),
    FrameRate::new(30, 1),
    FrameRate::new(50, 1),
    FrameRate::new(60_000, 1_001),
    FrameRate::new(60, 1),
];

/// The measured media data of a stream, as the passive capture stores it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeasuredStreamProfile {
    pub video_codec: Option<String>,
    pub video_width: Option<i64>,
    pub video_height: Option<i64>,
    pub video_fps: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<i64>,
    pub audio_sample_rate: Option<i64>,
}

/// The encode parameters of a slate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlateProfile {
    pub video: SlateVideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub audio: SlateAudioCodec,
    pub audio_channels: u8,
    pub sample_rate: u32,
}

impl Default for SlateProfile {
    /// The safe default: 1280x720 H.264 at 30 fps with 48 kHz stereo AAC.
    fn default() -> Self {
        Self {
            video: SlateVideoCodec::H264,
            width: 1_280,
            height: 720,
            frame_rate: FrameRate::new(30, 1),
            audio: SlateAudioCodec::Aac,
            audio_channels: 2,
            sample_rate: 48_000,
        }
    }
}

impl SlateProfile {
    /// Builds a slate profile from measured stream data.
    ///
    /// Each unknown or unsupported value gets the safe default value.
    #[must_use]
    pub fn from_measured(measured: &MeasuredStreamProfile) -> Self {
        let defaults = Self::default();
        let video = match measured.video_codec.as_deref() {
            Some("h265" | "hevc") => SlateVideoCodec::H265,
            Some("mpeg2-video" | "mpeg2video" | "mpeg1-video") => SlateVideoCodec::Mpeg2,
            _ => SlateVideoCodec::H264,
        };
        let (width, height) = match (measured.video_width, measured.video_height) {
            (Some(width), Some(height))
                if (128..=4_096).contains(&width) && (96..=2_304).contains(&height) =>
            {
                // The encoders need even dimensions for 4:2:0 video.
                (
                    u32::try_from(width).unwrap_or(defaults.width) & !1,
                    u32::try_from(height).unwrap_or(defaults.height) & !1,
                )
            }
            _ => (defaults.width, defaults.height),
        };
        let frame_rate = measured
            .video_fps
            .and_then(nearest_frame_rate)
            .unwrap_or(defaults.frame_rate);
        let audio = match measured.audio_codec.as_deref() {
            Some("ac3") => SlateAudioCodec::Ac3,
            Some("eac3") => SlateAudioCodec::Eac3,
            Some("mp2" | "mpeg1-audio") => SlateAudioCodec::Mp2,
            _ => SlateAudioCodec::Aac,
        };
        let audio_channels = match (measured.audio_channels, audio) {
            (Some(1), _) => 1,
            (Some(channels), codec) if channels >= 6 && codec != SlateAudioCodec::Mp2 => 6,
            _ => 2,
        };
        let sample_rate = match measured.audio_sample_rate {
            Some(rate @ (32_000 | 44_100 | 48_000)) => u32::try_from(rate).unwrap_or(48_000),
            _ => defaults.sample_rate,
        };
        Self {
            video,
            width,
            height,
            frame_rate,
            audio,
            audio_channels,
            sample_rate,
        }
    }
}

fn nearest_frame_rate(fps: f64) -> Option<FrameRate> {
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    FRAME_RATES
        .iter()
        .copied()
        .map(|rate| (rate, (rate.as_f64() - fps).abs() / fps))
        .filter(|(_, error)| *error <= 0.01)
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(rate, _)| rate)
}

// ---------------------------------------------------------------------------
// Slate kind and FFmpeg arguments
// ---------------------------------------------------------------------------

/// The message that a slate shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlateKind {
    /// "Stream starting in N seconds" with a countdown.
    Starting,
    /// "All tuners in use, stay tuned for one to open up".
    TunersBusy,
    /// "Having trouble reaching this channel, retrying".
    Retrying,
}

impl SlateKind {
    /// The text lines of the slate. The starting slate has a countdown
    /// expression for the `FFmpeg` `drawtext` filter.
    fn lines(self, countdown_seconds: u32) -> Vec<String> {
        match self {
            Self::Starting => vec![format!(
                "Stream starting in %{{eif\\:max(1\\,ceil({countdown_seconds}-t))\\:d}} seconds"
            )],
            Self::TunersBusy => vec![
                "All tuners in use,".to_owned(),
                "stay tuned for one to open up".to_owned(),
            ],
            Self::Retrying => vec![
                "Having trouble reaching this channel,".to_owned(),
                "retrying".to_owned(),
            ],
        }
    }
}

fn video_filter(kind: SlateKind, countdown_seconds: u32, profile: &SlateProfile) -> String {
    let font_size = (profile.height / 14).min(profile.width / 28).max(12);
    let step = font_size * 3 / 2;
    let lines = kind.lines(countdown_seconds);
    let count = u32::try_from(lines.len()).unwrap_or(1);
    let top = (profile.height / 2).saturating_sub(step * count / 2);
    lines
        .iter()
        .zip(0_u32..)
        .map(|(line, index)| {
            format!(
                "drawtext=text='{line}':fontcolor=white:fontsize={font_size}:x=(w-text_w)/2:y={}",
                top + step * index
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn channel_layout(channels: u8) -> &'static str {
    match channels {
        1 => "mono",
        6 => "5.1",
        _ => "stereo",
    }
}

/// The encoder arguments of the slate video.
fn video_encoder_arguments(codec: SlateVideoCodec, gop: String) -> Vec<String> {
    let mut arguments = Vec::new();
    match codec {
        SlateVideoCodec::H264 => arguments.extend(
            [
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-tune",
                "zerolatency",
                "-profile:v",
                "high",
                "-sc_threshold",
                "0",
                "-keyint_min",
            ]
            .map(String::from)
            .into_iter()
            .chain([gop]),
        ),
        SlateVideoCodec::H265 => arguments.extend(
            [
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-x265-params",
                "log-level=error",
            ]
            .map(String::from),
        ),
        SlateVideoCodec::Mpeg2 => {
            arguments.extend(["-c:v", "mpeg2video", "-q:v", "4", "-bf", "0"].map(String::from));
        }
    }
    arguments
}

/// Builds the fixed `FFmpeg` arguments of one slate.
///
/// All values come from enums and range-checked numbers. No text from a
/// provider or a client enters the arguments.
pub(crate) fn ffmpeg_arguments(
    kind: SlateKind,
    countdown_seconds: u32,
    profile: &SlateProfile,
) -> Vec<OsString> {
    let rate = profile.frame_rate;
    let gop = rate.keyframe_interval().to_string();
    let mut arguments: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-re".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "color=c=0x08111f:s={}x{}:r={}/{}",
            profile.width, profile.height, rate.numerator, rate.denominator
        ),
        "-re".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "anullsrc=channel_layout={}:sample_rate={}",
            channel_layout(profile.audio_channels),
            profile.sample_rate
        ),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "1:a:0".into(),
        "-vf".into(),
        video_filter(kind, countdown_seconds.max(1), profile),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-g".into(),
        gop.clone(),
    ];
    arguments.extend(video_encoder_arguments(profile.video, gop));
    let surround = profile.audio_channels >= 6;
    let (encoder, bitrate) = match profile.audio {
        SlateAudioCodec::Aac => ("aac", if surround { "384k" } else { "128k" }),
        SlateAudioCodec::Ac3 => ("ac3", if surround { "448k" } else { "192k" }),
        SlateAudioCodec::Eac3 => ("eac3", if surround { "448k" } else { "192k" }),
        SlateAudioCodec::Mp2 => ("mp2", "192k"),
    };
    arguments.extend(
        [
            "-c:a",
            encoder,
            "-b:a",
            bitrate,
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-flush_packets",
            "1",
            "-mpegts_service_id",
            "1",
            "-mpegts_pmt_start_pid",
            "4096",
            "-mpegts_start_pid",
            "256",
            "-f",
            "mpegts",
            "pipe:1",
        ]
        .map(String::from),
    );
    arguments.into_iter().map(OsString::from).collect()
}

// ---------------------------------------------------------------------------
// Slate selection and retry rules
// ---------------------------------------------------------------------------

/// The class of a session start error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartFailure {
    /// The provider pool has no free connection.
    Capacity,
    /// The provider or the input adapter failed. A retry can succeed.
    Provider,
    /// The relay configuration is not valid. A retry cannot succeed.
    Configuration,
}

impl StartFailure {
    /// Classifies one session start error.
    #[must_use]
    pub fn classify(error: &SessionStartError) -> Self {
        match error {
            SessionStartError::Provider(AcquireError::AtCapacity { .. }) => Self::Capacity,
            SessionStartError::StartupTimeout
            | SessionStartError::Http { .. }
            | SessionStartError::Process { .. } => Self::Provider,
            SessionStartError::UnknownProvider { .. }
            | SessionStartError::Provider(AcquireError::LeaseIdExhausted)
            | SessionStartError::ProcessInputRequiresProcessAdapter => Self::Configuration,
        }
    }

    /// The slate for this failure.
    #[must_use]
    pub fn slate(self) -> SlateKind {
        match self {
            Self::Capacity => SlateKind::TunersBusy,
            Self::Provider | Self::Configuration => SlateKind::Retrying,
        }
    }
}

/// The retry intervals of one viewer.
///
/// A full pool gets a fixed 500 ms retry. A provider error gets an
/// exponential back-off from 1 s to 30 s. Each retry is one `try_open`, so a
/// viewer never holds more than one provider connection attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetrySchedule {
    provider_failures: u32,
}

impl RetrySchedule {
    /// Returns the delay before the next attempt and records the failure.
    pub fn next_delay(&mut self, failure: StartFailure) -> Duration {
        match failure {
            StartFailure::Capacity => CAPACITY_RETRY_INTERVAL,
            StartFailure::Provider | StartFailure::Configuration => {
                let exponent = self.provider_failures.min(16);
                self.provider_failures = self.provider_failures.saturating_add(1);
                PROVIDER_RETRY_INITIAL
                    .saturating_mul(1_u32 << exponent)
                    .min(PROVIDER_RETRY_MAX)
            }
        }
    }

    /// Clears the back-off after live output starts.
    pub fn reset(&mut self) {
        self.provider_failures = 0;
    }
}

/// The start state of a viewer that has not sent media yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewerStartupStatus {
    /// True when media exists after the viewer position.
    pub buffered: bool,
    /// Media time between the viewer position and the newest data.
    pub lead: Option<Duration>,
    /// True when the shared ring is closed.
    pub closed: bool,
}

/// The decision for a viewer that waits for live media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    /// Send live media now.
    Ready,
    /// Show the starting slate. `remaining` is the countdown estimate.
    Waiting { remaining: Duration },
    /// The live session failed before it sent media.
    Failed,
}

/// The lead that a new viewer waits for: the paced delay, at most 5 s.
///
/// A zero delay (unpaced output) waits only for the first media.
#[must_use]
pub fn start_lead_target(viewer_delay: Duration) -> Duration {
    viewer_delay.min(SLATE_START_LEAD)
}

/// Decides if a viewer can start live output.
#[must_use]
pub fn viewer_readiness(
    target: Duration,
    status: ViewerStartupStatus,
    waited: Duration,
) -> Readiness {
    if status.closed {
        return Readiness::Failed;
    }
    if !status.buffered {
        if waited >= NO_MEDIA_LIMIT {
            return Readiness::Failed;
        }
        return Readiness::Waiting {
            remaining: target.max(STARTING_COUNTDOWN_FLOOR),
        };
    }
    if target.is_zero() {
        return Readiness::Ready;
    }
    match status.lead {
        Some(lead) if lead >= target => Readiness::Ready,
        Some(_) if waited >= target + LEAD_WAIT_LIMIT => Readiness::Ready,
        Some(lead) => Readiness::Waiting {
            remaining: target.saturating_sub(lead),
        },
        None if waited >= target => Readiness::Ready,
        None => Readiness::Waiting {
            remaining: target.saturating_sub(waited),
        },
    }
}

fn countdown_seconds(remaining: Duration) -> u32 {
    u32::try_from(remaining.as_millis().div_ceil(1_000))
        .unwrap_or(u32::MAX)
        .max(1)
}

// ---------------------------------------------------------------------------
// Splice muxer
// ---------------------------------------------------------------------------

const PAT_PID: u16 = 0;
const NULL_PID: u16 = 0x1fff;
const PROGRAM_NUMBER: u16 = 1;
const TRANSPORT_STREAM_ID: u16 = 1;
const TIMESTAMP_MASK: u64 = (1 << 33) - 1;
const TIMESTAMP_HALF_RANGE: u64 = 1 << 32;
/// The gap between the last output timestamp and the first spliced one:
/// one frame at 29.97 fps in 90 kHz units.
const SPLICE_STEP_TICKS: u64 = 3_003;
/// The largest PES step that counts as the duration of one PES.
const MAX_PES_STEP_TICKS: u64 = 90_000;
/// A splice without a marked keyframe starts at a video PES after this time.
const KEYFRAME_FALLBACK_TICKS: u64 = 3 * 90_000;
/// A splice without a PCR uses the PES timestamps after this many packets.
const NO_PCR_PACKET_BUDGET: u64 = 20_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Role {
    Video,
    Audio,
    Pcr,
}

impl Role {
    const ALL: [Self; 3] = [Self::Video, Self::Audio, Self::Pcr];

    const fn output_pid(self) -> u16 {
        match self {
            Self::Video => SLATE_VIDEO_PID,
            Self::Audio => SLATE_AUDIO_PID,
            Self::Pcr => SLATE_PCR_PID,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Video => 0,
            Self::Audio => 1,
            Self::Pcr => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EsEntry {
    pid: u16,
    stream_type: u8,
    descriptors: Vec<u8>,
}

/// The first video stream, the first audio stream, and the PCR PID of one
/// input program. The muxer drops all other streams, so the audio layout
/// stays the same between slate and live.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ProgramMap {
    video: Option<EsEntry>,
    audio: Option<EsEntry>,
    pcr_pid: Option<u16>,
}

fn is_video_stream_type(stream_type: u8) -> bool {
    matches!(stream_type, 0x01 | 0x02 | 0x10 | 0x1b | 0x24)
}

fn is_audio_stream(stream_type: u8, descriptors: &[u8]) -> bool {
    match stream_type {
        0x03 | 0x04 | 0x0f | 0x11 | 0x81 | 0x82 | 0x83 | 0x84 | 0x87 => true,
        // DVB private data: AC-3, E-AC-3, DTS, and AAC descriptors.
        0x06 => descriptor_tags(descriptors).any(|tag| matches!(tag, 0x6a | 0x7a | 0x7b | 0x7c)),
        _ => false,
    }
}

fn descriptor_tags(descriptors: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut offset = 0;
    std::iter::from_fn(move || {
        let tag = *descriptors.get(offset)?;
        let length = usize::from(*descriptors.get(offset + 1)?);
        offset += 2 + length;
        Some(tag)
    })
}

impl ProgramMap {
    fn from_pmt(section: &[u8]) -> Option<Self> {
        if section.len() < 16 || section[0] != 0x02 {
            return None;
        }
        let pcr_pid = (u16::from(section[8] & 0x1f) << 8) | u16::from(section[9]);
        let program_info_length = (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
        let streams_end = section.len().checked_sub(4)?;
        let mut offset = 12_usize.checked_add(program_info_length)?;
        let mut map = Self {
            pcr_pid: (pcr_pid != NULL_PID).then_some(pcr_pid),
            ..Self::default()
        };
        while offset + 5 <= streams_end {
            let stream_type = section[offset];
            let pid = (u16::from(section[offset + 1] & 0x1f) << 8) | u16::from(section[offset + 2]);
            let info_length =
                (usize::from(section[offset + 3] & 0x0f) << 8) | usize::from(section[offset + 4]);
            let next = offset + 5 + info_length;
            let descriptors = section.get(offset + 5..next)?;
            if next > streams_end {
                return None;
            }
            let entry = EsEntry {
                pid,
                stream_type,
                descriptors: descriptors.to_vec(),
            };
            if map.video.is_none() && is_video_stream_type(stream_type) {
                map.video = Some(entry);
            } else if map.audio.is_none() && is_audio_stream(stream_type, descriptors) {
                map.audio = Some(entry);
            }
            offset = next;
        }
        (map.video.is_some() || map.audio.is_some()).then_some(map)
    }

    fn role_of(&self, pid: u16) -> Option<Role> {
        if self.video.as_ref().is_some_and(|entry| entry.pid == pid) {
            Some(Role::Video)
        } else if self.audio.as_ref().is_some_and(|entry| entry.pid == pid) {
            Some(Role::Audio)
        } else if self.pcr_pid == Some(pid) {
            Some(Role::Pcr)
        } else {
            None
        }
    }

    fn roles(&self) -> impl Iterator<Item = Role> + '_ {
        Role::ALL.into_iter().filter(|role| match role {
            Role::Video => self.video.is_some(),
            Role::Audio => self.audio.is_some(),
            Role::Pcr => self
                .pcr_pid
                .is_some_and(|pid| self.role_of(pid) == Some(Role::Pcr)),
        })
    }

    fn output_pcr_pid(&self) -> u16 {
        self.pcr_pid
            .and_then(|pid| self.role_of(pid))
            .map_or(NULL_PID, Role::output_pid)
    }
}

#[derive(Debug, Default)]
struct Segment {
    tracker: PatPmtTracker,
    map: Option<ProgramMap>,
    started: bool,
    packets_seen: u64,
    first_pcr: Option<u64>,
    latest_pcr: Option<u64>,
    latest_pts: Option<u64>,
    offset: u64,
    cc_delta: [Option<u8>; 3],
    role_started: [bool; 3],
}

impl Segment {
    /// Feeds the PSI tracker. Returns a new program map when the input PMT
    /// changed.
    fn observe_psi(&mut self, packet: &[u8]) -> Option<ProgramMap> {
        let sequence = self.packets_seen;
        self.packets_seen += 1;
        self.tracker.observe(packet, sequence)?;
        let (section, _) = self.tracker.completed_pmt()?;
        let map = ProgramMap::from_pmt(section)?;
        (self.map.as_ref() != Some(&map)).then_some(map)
    }
}

/// Joins slate and live MPEG-TS segments into one continuous program.
///
/// Output rules:
///
/// - Video uses PID `0x100`, audio uses PID `0x101`, and the PMT uses PID
///   `0x1000`. Other input streams are dropped. Null packets pass through.
/// - Each new segment starts with a new PAT, a PMT with a new version, and an
///   adaptation-only packet with `discontinuity_indicator` on each stream PID.
/// - Video starts at a keyframe. Audio starts at the next PES start.
/// - PCR, PTS, and DTS values are rebased, so the first value of a new
///   segment comes one frame after the estimated end of the output media.
/// - Continuity counters continue from the previous segment and keep the
///   input gaps.
#[derive(Debug, Default)]
pub struct SpliceMuxer {
    current: Option<Segment>,
    pending: Option<Segment>,
    output_cc: [Option<u8>; 3],
    pat_cc: u8,
    pmt_cc: u8,
    pmt_version: u8,
    output_map: Option<ProgramMap>,
    last_pcr: Option<u64>,
    /// The latest PTS or DTS of each output stream.
    pes_time: [Option<u64>; 3],
    /// The last step between two PES starts of each output stream. One PES
    /// can hold more than one audio frame, so the media of a PES continues
    /// for about one step after its timestamp.
    pes_step: [u64; 3],
    /// The estimated end of all output media.
    media_end: Option<u64>,
    splices: u64,
}

impl SpliceMuxer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of segments that started.
    #[must_use]
    pub fn splices(&self) -> u64 {
        self.splices
    }

    /// True while a new segment waits for its start point.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Prepares a new segment. The current segment continues until the new
    /// segment finds its start point. A previous pending segment is dropped.
    pub fn begin_pending(&mut self) {
        self.pending = Some(Segment::default());
    }

    /// Drops the pending segment.
    pub fn cancel_pending(&mut self) {
        self.pending = None;
    }

    /// Processes packets of the current segment.
    pub fn push_current(&mut self, chunk: &[u8], out: &mut BytesMut) {
        let Some(mut segment) = self.current.take() else {
            return;
        };
        for packet in chunk.chunks_exact(MPEG_TS_PACKET_SIZE) {
            self.running_packet(&mut segment, packet, out);
        }
        self.current = Some(segment);
    }

    /// Processes packets of the pending segment.
    ///
    /// Returns true when the pending segment found its start point and became
    /// the current segment. The output then has the splice packets and the
    /// first packets of the new segment.
    pub fn push_pending(&mut self, chunk: &[u8], out: &mut BytesMut) -> bool {
        let Some(mut segment) = self.pending.take() else {
            return false;
        };
        for packet in chunk.chunks_exact(MPEG_TS_PACKET_SIZE) {
            if segment.started {
                self.running_packet(&mut segment, packet, out);
            } else if self.seek(&mut segment, packet) {
                self.start_segment(&mut segment, out);
                self.running_packet(&mut segment, packet, out);
            }
        }
        if segment.started {
            self.current = Some(segment);
            true
        } else {
            self.pending = Some(segment);
            false
        }
    }

    /// Looks for the start point of a pending segment. Returns true when
    /// `packet` is the first packet to send.
    fn seek(&self, segment: &mut Segment, packet: &[u8]) -> bool {
        if packet.len() != MPEG_TS_PACKET_SIZE || packet[0] != 0x47 {
            return false;
        }
        if let Some(map) = segment.observe_psi(packet) {
            segment.map = Some(map);
        }
        let Some(map) = segment.map.as_ref() else {
            return false;
        };
        let pid = packet_pid(packet);
        if map.pcr_pid == Some(pid)
            && let Some(pcr) = read_pcr_base(packet)
        {
            segment.latest_pcr = Some(pcr);
            segment.first_pcr.get_or_insert(pcr);
        }
        let start = payload_unit_start(packet);
        if start
            && map.role_of(pid).is_some()
            && let Some(pts) = read_pes_pts(packet)
        {
            segment.latest_pts = Some(pts);
        }
        let reference = segment.latest_pcr.or_else(|| {
            (map.pcr_pid.is_none() || segment.packets_seen >= NO_PCR_PACKET_BUDGET)
                .then_some(segment.latest_pts?.wrapping_sub(90_000) & TIMESTAMP_MASK)
        });
        let Some(reference) = reference else {
            return false;
        };
        if !start {
            return false;
        }
        let accept = match &map.video {
            Some(video) if video.pid == pid => {
                let waited = match (segment.first_pcr, segment.latest_pcr) {
                    (Some(first), Some(latest)) => {
                        latest.wrapping_sub(first) & TIMESTAMP_MASK >= KEYFRAME_FALLBACK_TICKS
                    }
                    _ => segment.packets_seen >= NO_PCR_PACKET_BUDGET,
                };
                waited || is_keyframe(packet, video.stream_type)
            }
            Some(_) => false,
            None => map.audio.as_ref().is_some_and(|audio| audio.pid == pid),
        };
        if accept {
            segment.offset = self.offset_for(reference);
        }
        accept
    }

    /// The rebase offset that maps `input_reference` one frame after the
    /// last output timestamp.
    fn offset_for(&self, input_reference: u64) -> u64 {
        match self.output_reference() {
            Some(last) => (last + SPLICE_STEP_TICKS).wrapping_sub(input_reference) & TIMESTAMP_MASK,
            None => 0,
        }
    }

    fn output_reference(&self) -> Option<u64> {
        match (self.last_pcr, self.media_end) {
            (Some(pcr), Some(pts)) => Some(later_timestamp(pcr, pts)),
            (pcr, pts) => pcr.or(pts),
        }
    }

    fn start_segment(&mut self, segment: &mut Segment, out: &mut BytesMut) {
        let had_output = self.output_map.is_some();
        segment.started = true;
        self.splices += 1;
        self.pmt_version = (self.pmt_version + 1) & 0x1f;
        self.output_map.clone_from(&segment.map);
        self.write_pat(out);
        self.write_pmt(out);
        if had_output && let Some(map) = segment.map.as_ref() {
            for role in map.roles() {
                let index = role.index();
                // An adaptation-only packet does not increment the counter.
                let counter = self.output_cc[index].unwrap_or(0x0f);
                self.output_cc[index] = Some(counter);
                out.extend_from_slice(&discontinuity_packet(role.output_pid(), counter));
            }
        }
    }

    fn running_packet(&mut self, segment: &mut Segment, packet: &[u8], out: &mut BytesMut) {
        if packet.len() != MPEG_TS_PACKET_SIZE || packet[0] != 0x47 {
            return;
        }
        let pid = packet_pid(packet);
        if pid == NULL_PID {
            out.extend_from_slice(packet);
            return;
        }
        if let Some(map) = segment.observe_psi(packet) {
            // The input program changed. Send the new layout and restart each
            // stream at its next PES start.
            segment.map = Some(map);
            segment.cc_delta = [None; 3];
            segment.role_started = [false; 3];
            self.pmt_version = (self.pmt_version + 1) & 0x1f;
            self.output_map.clone_from(&segment.map);
            self.write_pmt(out);
        }
        if pid == PAT_PID {
            if payload_unit_start(packet) {
                self.write_pat(out);
                self.write_pmt(out);
            }
            return;
        }
        let Some(role) = segment.map.as_ref().and_then(|map| map.role_of(pid)) else {
            return;
        };
        let index = role.index();
        if !segment.role_started[index] {
            if role != Role::Pcr && !payload_unit_start(packet) {
                return;
            }
            segment.role_started[index] = true;
        }
        let mut output = [0_u8; MPEG_TS_PACKET_SIZE];
        output.copy_from_slice(packet);
        let output_pid = role.output_pid();
        output[1] = (output[1] & 0xe0) | ((output_pid >> 8) as u8 & 0x1f);
        output[2] = (output_pid & 0xff) as u8;

        let input_counter = output[3] & 0x0f;
        let has_payload = output[3] & 0x10 != 0;
        let delta = *segment.cc_delta[index].get_or_insert_with(|| {
            let target = match self.output_cc[index] {
                Some(last) if has_payload => (last + 1) & 0x0f,
                Some(last) => last,
                None => input_counter,
            };
            target.wrapping_sub(input_counter) & 0x0f
        });
        let counter = (input_counter + delta) & 0x0f;
        output[3] = (output[3] & 0xf0) | counter;
        self.output_cc[index] = Some(counter);

        if let Some(base) = read_pcr_base(&output) {
            let rebased = (base + segment.offset) & TIMESTAMP_MASK;
            write_pcr_base(&mut output, rebased);
            self.last_pcr = Some(rebased);
        }
        if payload_unit_start(&output)
            && let Some(latest) = rebase_pes_timestamps(&mut output, segment.offset)
        {
            if let Some(previous) = self.pes_time[index] {
                let step = latest.wrapping_sub(previous) & TIMESTAMP_MASK;
                if step > 0 && step <= MAX_PES_STEP_TICKS {
                    self.pes_step[index] = step;
                }
            }
            self.pes_time[index] = Some(
                self.pes_time[index].map_or(latest, |previous| later_timestamp(previous, latest)),
            );
            let end = (latest + self.pes_step[index]) & TIMESTAMP_MASK;
            self.media_end = Some(
                self.media_end
                    .map_or(end, |previous| later_timestamp(previous, end)),
            );
        }
        out.extend_from_slice(&output);
    }

    fn write_pat(&mut self, out: &mut BytesMut) {
        let mut section = vec![
            0x00,
            0xb0,
            0x0d,
            (TRANSPORT_STREAM_ID >> 8) as u8,
            (TRANSPORT_STREAM_ID & 0xff) as u8,
            0xc1,
            0x00,
            0x00,
            (PROGRAM_NUMBER >> 8) as u8,
            (PROGRAM_NUMBER & 0xff) as u8,
            0xe0 | (SLATE_PMT_PID >> 8) as u8,
            (SLATE_PMT_PID & 0xff) as u8,
        ];
        append_crc(&mut section);
        out.extend_from_slice(&psi_packet(PAT_PID, self.pat_cc, &section));
        self.pat_cc = (self.pat_cc + 1) & 0x0f;
    }

    fn write_pmt(&mut self, out: &mut BytesMut) {
        let Some(map) = self.output_map.as_ref() else {
            return;
        };
        let section = pmt_section(map, self.pmt_version);
        out.extend_from_slice(&psi_packet(SLATE_PMT_PID, self.pmt_cc, &section));
        self.pmt_cc = (self.pmt_cc + 1) & 0x0f;
    }
}

/// The largest PSI section that fits in one packet after the pointer field.
const MAX_SINGLE_PACKET_SECTION: usize = MPEG_TS_PACKET_SIZE - 5;

fn pmt_section(map: &ProgramMap, version: u8) -> Vec<u8> {
    let entries: Vec<(Role, &EsEntry)> = [
        (Role::Video, map.video.as_ref()),
        (Role::Audio, map.audio.as_ref()),
    ]
    .into_iter()
    .filter_map(|(role, entry)| entry.map(|entry| (role, entry)))
    .collect();
    let fixed = 12 + 4 + 5 * entries.len();
    let descriptor_bytes: usize = entries
        .iter()
        .map(|(_, entry)| entry.descriptors.len())
        .sum();
    // Drop the input descriptors when the section does not fit in one packet.
    let keep_descriptors = fixed + descriptor_bytes <= MAX_SINGLE_PACKET_SECTION;
    let mut section = vec![
        0x02,
        0xb0,
        0x00,
        (PROGRAM_NUMBER >> 8) as u8,
        (PROGRAM_NUMBER & 0xff) as u8,
        0xc1 | ((version & 0x1f) << 1),
        0x00,
        0x00,
    ];
    let pcr_pid = map.output_pcr_pid();
    section.extend_from_slice(&[
        0xe0 | (pcr_pid >> 8) as u8,
        (pcr_pid & 0xff) as u8,
        0xf0,
        0x00,
    ]);
    for (role, entry) in entries {
        let pid = role.output_pid();
        let descriptors: &[u8] = if keep_descriptors {
            &entry.descriptors
        } else {
            &[]
        };
        let length = u16::try_from(descriptors.len()).unwrap_or(0);
        section.extend_from_slice(&[
            entry.stream_type,
            0xe0 | (pid >> 8) as u8,
            (pid & 0xff) as u8,
            0xf0 | (length >> 8) as u8,
            (length & 0xff) as u8,
        ]);
        section.extend_from_slice(descriptors);
    }
    let section_length = u16::try_from(section.len() - 3 + 4).unwrap_or(0);
    section[1] = 0xb0 | ((section_length >> 8) as u8 & 0x0f);
    section[2] = (section_length & 0xff) as u8;
    append_crc(&mut section);
    section
}

fn append_crc(section: &mut Vec<u8>) {
    let crc = mpeg_crc32(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

fn psi_packet(pid: u16, counter: u8, section: &[u8]) -> [u8; MPEG_TS_PACKET_SIZE] {
    let mut packet = [0xff_u8; MPEG_TS_PACKET_SIZE];
    packet[0] = 0x47;
    packet[1] = 0x40 | ((pid >> 8) as u8 & 0x1f);
    packet[2] = (pid & 0xff) as u8;
    packet[3] = 0x10 | (counter & 0x0f);
    packet[4] = 0x00;
    let length = section.len().min(MAX_SINGLE_PACKET_SECTION);
    packet[5..5 + length].copy_from_slice(&section[..length]);
    packet
}

/// An adaptation-only packet with `discontinuity_indicator` set.
fn discontinuity_packet(pid: u16, counter: u8) -> [u8; MPEG_TS_PACKET_SIZE] {
    let mut packet = [0xff_u8; MPEG_TS_PACKET_SIZE];
    packet[0] = 0x47;
    packet[1] = (pid >> 8) as u8 & 0x1f;
    packet[2] = (pid & 0xff) as u8;
    packet[3] = 0x20 | (counter & 0x0f);
    packet[4] = 183;
    packet[5] = 0x80;
    packet
}

fn packet_pid(packet: &[u8]) -> u16 {
    (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
}

fn payload_unit_start(packet: &[u8]) -> bool {
    packet[1] & 0x40 != 0
}

fn payload_offset(packet: &[u8]) -> Option<usize> {
    match (packet[3] >> 4) & 0x03 {
        1 => Some(4),
        3 => {
            let offset = 5 + usize::from(packet[4]);
            (offset < MPEG_TS_PACKET_SIZE).then_some(offset)
        }
        _ => None,
    }
}

fn has_pcr(packet: &[u8]) -> bool {
    packet[3] & 0x20 != 0 && packet[4] >= 7 && packet[5] & 0x10 != 0
}

fn read_pcr_base(packet: &[u8]) -> Option<u64> {
    if !has_pcr(packet) {
        return None;
    }
    Some(
        (u64::from(packet[6]) << 25)
            | (u64::from(packet[7]) << 17)
            | (u64::from(packet[8]) << 9)
            | (u64::from(packet[9]) << 1)
            | (u64::from(packet[10]) >> 7),
    )
}

/// Returns the low eight bits of a value.
fn low_byte(value: u64) -> u8 {
    value.to_le_bytes()[0]
}

fn write_pcr_base(packet: &mut [u8], base: u64) {
    packet[6] = low_byte(base >> 25);
    packet[7] = low_byte(base >> 17);
    packet[8] = low_byte(base >> 9);
    packet[9] = low_byte(base >> 1);
    packet[10] = (low_byte(base & 1) << 7) | (packet[10] & 0x7f);
}

fn is_random_access(packet: &[u8]) -> bool {
    packet[3] & 0x20 != 0 && packet[4] > 0 && packet[5] & 0x40 != 0
}

/// Returns the PES header range: the start of the PES packet and the length
/// of its optional header. `None` when the PES has no optional header.
fn pes_header(packet: &[u8]) -> Option<(usize, usize)> {
    let start = payload_offset(packet)?;
    let pes = &packet[start..];
    if pes.len() < 9 || pes[..3] != [0x00, 0x00, 0x01] {
        return None;
    }
    if matches!(
        pes[3],
        0xbc | 0xbe | 0xbf | 0xf0 | 0xf1 | 0xf2 | 0xf8 | 0xff
    ) {
        return None;
    }
    Some((start, usize::from(pes[8])))
}

fn read_timestamp(bytes: &[u8]) -> u64 {
    (u64::from((bytes[0] >> 1) & 0x07) << 30)
        | (u64::from(bytes[1]) << 22)
        | (u64::from(bytes[2] >> 1) << 15)
        | (u64::from(bytes[3]) << 7)
        | (u64::from(bytes[4]) >> 1)
}

fn write_timestamp(bytes: &mut [u8], value: u64) {
    bytes[0] = (bytes[0] & 0xf0) | (low_byte(value >> 29) & 0x0e) | 0x01;
    bytes[1] = low_byte(value >> 22);
    bytes[2] = (low_byte(value >> 14) & 0xfe) | 0x01;
    bytes[3] = low_byte(value >> 7);
    bytes[4] = (low_byte(value << 1) & 0xfe) | 0x01;
}

fn read_pes_pts(packet: &[u8]) -> Option<u64> {
    let (start, _) = pes_header(packet)?;
    let pes = &packet[start..];
    (pes[7] & 0x80 != 0 && pes.len() >= 14).then(|| read_timestamp(&pes[9..14]))
}

/// Adds `offset` to the PTS and DTS of a PES start. Returns the latest
/// rebased timestamp.
fn rebase_pes_timestamps(packet: &mut [u8], offset: u64) -> Option<u64> {
    let (start, _) = pes_header(packet)?;
    let flags = packet[start + 7] >> 6;
    let mut latest = None;
    for (present, range) in [
        (flags & 0x02 != 0, start + 9..start + 14),
        (flags == 0x03, start + 14..start + 19),
    ] {
        if !present || range.end > MPEG_TS_PACKET_SIZE {
            continue;
        }
        let field = &mut packet[range];
        let rebased = (read_timestamp(field) + offset) & TIMESTAMP_MASK;
        write_timestamp(field, rebased);
        latest = Some(latest.map_or(rebased, |previous| later_timestamp(previous, rebased)));
    }
    latest
}

/// Returns the later of two 33-bit timestamps, with wrap.
fn later_timestamp(left: u64, right: u64) -> u64 {
    if left.wrapping_sub(right) & TIMESTAMP_MASK < TIMESTAMP_HALF_RANGE {
        left
    } else {
        right
    }
}

/// True when a video PES start begins a decodable picture.
fn is_keyframe(packet: &[u8], stream_type: u8) -> bool {
    if is_random_access(packet) {
        return true;
    }
    let Some((start, header_length)) = pes_header(packet) else {
        return false;
    };
    let Some(payload) = packet.get(start + 9 + header_length..) else {
        return false;
    };
    payload.windows(4).any(|window| {
        if window[..3] != [0x00, 0x00, 0x01] {
            return false;
        }
        let code = window[3];
        match stream_type {
            // H.264: IDR slice or sequence parameter set.
            0x1b => matches!(code & 0x1f, 5 | 7),
            // H.265: IRAP picture or a parameter set.
            0x24 => matches!((code >> 1) & 0x3f, 16..=21 | 32..=34),
            // MPEG-1 and MPEG-2: sequence header.
            0x01 | 0x02 => code == 0xb3,
            _ => false,
        }
    })
}

// ---------------------------------------------------------------------------
// Slated open
// ---------------------------------------------------------------------------

/// A callback for each live viewer that the slate path opens.
pub type ViewerHook = Arc<dyn Fn(&ViewerHandle) + Send + Sync>;

/// Options for [`open_with_slates`].
#[derive(Clone)]
pub struct SlateOptions {
    profile: SlateProfile,
    grace: Duration,
    viewer_hook: Option<ViewerHook>,
}

impl fmt::Debug for SlateOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlateOptions")
            .field("profile", &self.profile)
            .field("grace", &self.grace)
            .field("viewer_hook", &self.viewer_hook.is_some())
            .finish()
    }
}

impl SlateOptions {
    #[must_use]
    pub fn new(profile: SlateProfile) -> Self {
        Self {
            profile,
            grace: DEFAULT_SLATE_GRACE,
            viewer_hook: None,
        }
    }

    /// Sets the time that a request waits for live media before a slate.
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Sets a callback for each live viewer that opens.
    #[must_use]
    pub fn with_viewer_hook(mut self, hook: ViewerHook) -> Self {
        self.viewer_hook = Some(hook);
        self
    }
}

/// The result of [`open_with_slates`].
pub enum SlatedOpen {
    /// The live viewer had media within the grace time. No slate is necessary.
    Direct(ViewerHandle),
    /// The response starts with a slate and splices to live media later.
    Slated(SlatedStream),
    /// The relay configuration is not valid. A retry cannot succeed.
    Failed(SessionStartError),
}

impl fmt::Debug for SlatedOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(viewer) => formatter.debug_tuple("Direct").field(viewer).finish(),
            Self::Slated(_) => formatter.write_str("Slated"),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
        }
    }
}

/// An MPEG-TS byte stream that starts with a slate and continues with live
/// media.
pub struct SlatedStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
}

impl fmt::Debug for SlatedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlatedStream")
            .finish_non_exhaustive()
    }
}

impl Stream for SlatedStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

type OpenFuture = Pin<Box<dyn Future<Output = Result<ViewerHandle, SessionStartError>> + Send>>;

fn open_future(media: &HttpTsSessionManager, source: &HttpTsSourceSpec) -> OpenFuture {
    let media = media.clone();
    let source = source.clone();
    Box::pin(async move { media.try_open(source).await })
}

/// Opens a channel and never returns an HTTP error for a provider problem.
///
/// The function waits at most the grace time. If the live viewer has media in
/// that time, the result is [`SlatedOpen::Direct`] and the output is the same
/// as without slates. Otherwise the result is a [`SlatedStream`]: it shows the
/// correct slate, retries the provider, and splices to live media.
pub async fn open_with_slates(
    media: HttpTsSessionManager,
    source: HttpTsSourceSpec,
    options: SlateOptions,
) -> SlatedOpen {
    let target = start_lead_target(source.viewer_delay());
    let deadline = Instant::now() + options.grace;
    let mut retry = RetrySchedule::default();
    let mut open = open_future(&media, &source);
    let first = tokio::select! {
        result = &mut open => Some(result),
        () = tokio::time::sleep_until(deadline) => None,
    };
    let live = match first {
        None => LiveState::Opening(open),
        Some(Err(error)) => {
            let failure = StartFailure::classify(&error);
            if failure == StartFailure::Configuration {
                return SlatedOpen::Failed(error);
            }
            LiveState::Idle {
                retry_at: Instant::now() + retry.next_delay(failure),
                failure,
            }
        }
        Some(Ok(viewer)) => {
            if let Some(hook) = &options.viewer_hook {
                hook(&viewer);
            }
            let since = Instant::now();
            loop {
                match viewer_readiness(target, viewer.startup_status(), since.elapsed()) {
                    Readiness::Ready => return SlatedOpen::Direct(viewer),
                    Readiness::Failed => {
                        break LiveState::Idle {
                            retry_at: Instant::now() + retry.next_delay(StartFailure::Provider),
                            failure: StartFailure::Provider,
                        };
                    }
                    Readiness::Waiting { .. } => {
                        let now = Instant::now();
                        if now >= deadline {
                            break LiveState::Waiting { viewer, since };
                        }
                        tokio::time::sleep((deadline - now).min(SLATE_TICK / 2)).await;
                    }
                }
            }
        }
    };
    let driver = SlateDriver {
        media,
        source,
        profile: options.profile,
        viewer_hook: options.viewer_hook,
        target,
        muxer: SpliceMuxer::new(),
        retry,
        live,
        active: None,
        incoming: None,
        shown: None,
        last_slate_start: None,
        finished: false,
    };
    SlatedOpen::Slated(driver.into_stream())
}

enum LiveState {
    /// A `try_open` call is in progress.
    Opening(OpenFuture),
    /// The live viewer exists, but its buffer is not ready.
    Waiting {
        viewer: ViewerHandle,
        since: Instant,
    },
    /// The next `try_open` call starts at `retry_at`.
    Idle {
        retry_at: Instant,
        failure: StartFailure,
    },
    /// Live media feeds the pending segment of the muxer.
    Splicing(ViewerByteStream),
    /// Live media feeds the current segment of the muxer.
    Live(ViewerByteStream),
}

enum LiveEvent {
    Opened(Result<ViewerHandle, SessionStartError>),
    Media(Option<Result<Bytes, ViewerStreamError>>),
}

async fn next_live_event(live: &mut LiveState) -> LiveEvent {
    match live {
        LiveState::Opening(future) => LiveEvent::Opened(future.await),
        LiveState::Splicing(stream) | LiveState::Live(stream) => {
            LiveEvent::Media(stream.next().await)
        }
        LiveState::Waiting { .. } | LiveState::Idle { .. } => pending().await,
    }
}

struct RunningSlate {
    kind: SlateKind,
    // Dropping the process requests its shutdown.
    _process: ProcessInputSession,
    cursor: RingCursor,
}

async fn next_slate_read(slot: &mut Option<RunningSlate>) -> Result<RingRead, RingReadError> {
    match slot {
        Some(slate) => slate.cursor.next(SLATE_BATCH_PACKETS).await,
        None => pending().await,
    }
}

enum DriverEvent {
    Active(Result<RingRead, RingReadError>),
    Incoming(Result<RingRead, RingReadError>),
    Live(LiveEvent),
    Tick,
}

struct SlateDriver {
    media: HttpTsSessionManager,
    source: HttpTsSourceSpec,
    profile: SlateProfile,
    viewer_hook: Option<ViewerHook>,
    target: Duration,
    muxer: SpliceMuxer,
    retry: RetrySchedule,
    live: LiveState,
    /// The slate that feeds the current muxer segment.
    active: Option<RunningSlate>,
    /// A new slate that feeds the pending muxer segment.
    incoming: Option<RunningSlate>,
    /// The latest requested slate.
    shown: Option<SlateKind>,
    last_slate_start: Option<Instant>,
    finished: bool,
}

impl SlateDriver {
    fn into_stream(mut self) -> SlatedStream {
        let inner = Box::pin(async_stream::stream! {
            let mut tick = tokio::time::interval(SLATE_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut out = BytesMut::new();
            while !self.finished {
                self.ensure_slate();
                let event = tokio::select! {
                    read = next_slate_read(&mut self.active) => DriverEvent::Active(read),
                    read = next_slate_read(&mut self.incoming) => DriverEvent::Incoming(read),
                    event = next_live_event(&mut self.live) => DriverEvent::Live(event),
                    _ = tick.tick() => DriverEvent::Tick,
                };
                match event {
                    DriverEvent::Active(read) => self.on_active_read(read, &mut out),
                    DriverEvent::Incoming(read) => self.on_incoming_read(read, &mut out),
                    DriverEvent::Live(event) => self.on_live_event(event, &mut out),
                    DriverEvent::Tick => self.on_tick(),
                }
                if !out.is_empty() {
                    yield Ok(out.split().freeze());
                }
            }
        });
        SlatedStream { inner }
    }

    /// The slate that the current live state needs.
    fn desired_slate(&self) -> Option<SlateKind> {
        match &self.live {
            LiveState::Live(_) => None,
            LiveState::Splicing(_) => self.shown,
            LiveState::Waiting { .. } => Some(SlateKind::Starting),
            LiveState::Opening(_) => Some(self.shown.unwrap_or(SlateKind::Starting)),
            LiveState::Idle { failure, .. } => Some(failure.slate()),
        }
    }

    fn countdown(&self) -> u32 {
        let remaining = match &self.live {
            LiveState::Waiting { viewer, since } => {
                match viewer_readiness(self.target, viewer.startup_status(), since.elapsed()) {
                    Readiness::Waiting { remaining } => remaining,
                    Readiness::Ready | Readiness::Failed => Duration::ZERO,
                }
            }
            _ => self.target.max(STARTING_COUNTDOWN_FLOOR),
        };
        countdown_seconds(remaining)
    }

    /// Starts a new slate when the needed slate changes.
    fn ensure_slate(&mut self) {
        let Some(kind) = self.desired_slate() else {
            return;
        };
        if self.shown == Some(kind) {
            return;
        }
        let now = Instant::now();
        if self
            .last_slate_start
            .is_some_and(|last| now.duration_since(last) < SLATE_RESTART_INTERVAL)
            && self.active.is_none()
            && self.incoming.is_none()
            && self.shown.is_none()
        {
            // A slate process failed a short time ago. Wait before a restart.
            return;
        }
        self.last_slate_start = Some(now);
        self.shown = Some(kind);
        let countdown = self.countdown();
        let ring = MpegTsRingConfig::new(SLATE_RING_PACKETS, SLATE_RING_PACKETS)
            .expect("the fixed slate ring configuration is valid");
        match FfmpegInputAdapter::new().start_slate(kind, countdown, &self.profile, ring) {
            Ok(process) => {
                info!(
                    channel = %self.source.key().source_id,
                    slate = ?kind,
                    countdown,
                    "stream slate started"
                );
                let cursor = process.subscribe();
                self.incoming = Some(RunningSlate {
                    kind,
                    _process: process,
                    cursor,
                });
                self.muxer.begin_pending();
            }
            Err(error) => {
                warn!(%error, slate = ?kind, "stream slate could not start");
                self.incoming = None;
                self.muxer.cancel_pending();
            }
        }
    }

    fn on_active_read(&mut self, read: Result<RingRead, RingReadError>, out: &mut BytesMut) {
        match read {
            Ok(RingRead::Packets { bytes, .. }) => self.muxer.push_current(&bytes, out),
            Ok(RingRead::Lagged { .. } | RingRead::GenerationBoundary { .. }) => {}
            Ok(RingRead::Closed { .. }) | Err(_) => {
                warn!("stream slate stopped");
                self.active = None;
                if self.incoming.is_none() {
                    self.shown = None;
                }
            }
        }
    }

    fn on_incoming_read(&mut self, read: Result<RingRead, RingReadError>, out: &mut BytesMut) {
        match read {
            Ok(RingRead::Packets { bytes, .. }) => {
                if self.muxer.push_pending(&bytes, out) {
                    self.active = self.incoming.take();
                }
            }
            Ok(RingRead::Lagged { .. } | RingRead::GenerationBoundary { .. }) => {}
            Ok(RingRead::Closed { .. }) | Err(_) => {
                warn!("stream slate stopped before its first frame");
                self.incoming = None;
                self.muxer.cancel_pending();
                self.shown = self.active.as_ref().map(|slate| slate.kind);
            }
        }
    }

    fn on_live_event(&mut self, event: LiveEvent, out: &mut BytesMut) {
        match event {
            LiveEvent::Opened(Ok(viewer)) => {
                if let Some(hook) = &self.viewer_hook {
                    hook(&viewer);
                }
                let since = Instant::now();
                match viewer_readiness(self.target, viewer.startup_status(), Duration::ZERO) {
                    Readiness::Ready => self.begin_splice(viewer),
                    Readiness::Waiting { .. } => self.live = LiveState::Waiting { viewer, since },
                    Readiness::Failed => self.fail(StartFailure::Provider),
                }
            }
            LiveEvent::Opened(Err(error)) => {
                let failure = StartFailure::classify(&error);
                if failure != StartFailure::Capacity {
                    warn!(%error, "live stream start failed; the slate retries");
                }
                self.fail(failure);
            }
            LiveEvent::Media(Some(Ok(bytes))) => match &self.live {
                LiveState::Splicing(_) => {
                    if self.muxer.push_pending(&bytes, out) {
                        let LiveState::Splicing(stream) = std::mem::replace(
                            &mut self.live,
                            LiveState::Idle {
                                retry_at: Instant::now(),
                                failure: StartFailure::Provider,
                            },
                        ) else {
                            unreachable!("the live state is Splicing");
                        };
                        self.live = LiveState::Live(stream);
                        self.active = None;
                        self.incoming = None;
                        self.shown = None;
                        self.retry.reset();
                        info!(
                            channel = %self.source.key().source_id,
                            splices = self.muxer.splices(),
                            "stream slate spliced to live media"
                        );
                    }
                }
                _ => self.muxer.push_current(&bytes, out),
            },
            LiveEvent::Media(Some(Err(error))) => {
                warn!(%error, "live stream failed; the slate retries");
                self.fail(StartFailure::Provider);
            }
            LiveEvent::Media(None) => {
                if matches!(self.live, LiveState::Live(_)) {
                    self.finished = true;
                } else {
                    self.fail(StartFailure::Provider);
                }
            }
        }
    }

    fn on_tick(&mut self) {
        match &self.live {
            LiveState::Waiting { viewer, since } => {
                match viewer_readiness(self.target, viewer.startup_status(), since.elapsed()) {
                    Readiness::Ready => {
                        let LiveState::Waiting { viewer, .. } = std::mem::replace(
                            &mut self.live,
                            LiveState::Idle {
                                retry_at: Instant::now(),
                                failure: StartFailure::Provider,
                            },
                        ) else {
                            unreachable!("the live state is Waiting");
                        };
                        self.begin_splice(viewer);
                    }
                    Readiness::Failed => self.fail(StartFailure::Provider),
                    Readiness::Waiting { .. } => {}
                }
            }
            LiveState::Idle { retry_at, .. } if Instant::now() >= *retry_at => {
                self.live = LiveState::Opening(open_future(&self.media, &self.source));
            }
            _ => {}
        }
    }

    fn begin_splice(&mut self, viewer: ViewerHandle) {
        // Live media takes the pending segment. A slate that has not started
        // yet is not necessary.
        self.incoming = None;
        if self.active.is_none() {
            self.shown = None;
        } else {
            self.shown = self.active.as_ref().map(|slate| slate.kind);
        }
        self.muxer.begin_pending();
        self.live = LiveState::Splicing(viewer.into_byte_stream());
    }

    fn fail(&mut self, failure: StartFailure) {
        // A failed live stream that fed the pending segment cannot finish it.
        if matches!(self.live, LiveState::Splicing(_)) {
            self.muxer.cancel_pending();
        }
        let delay = self.retry.next_delay(failure);
        self.live = LiveState::Idle {
            retry_at: Instant::now() + delay,
            failure,
        };
    }
}

#[cfg(test)]
mod tests;
