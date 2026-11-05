#![allow(clippy::cast_possible_truncation)]

use std::collections::HashMap;

use super::*;
use crate::{SessionInputAdapter, psi::test_support::psi_packet as input_psi_packet};

// ---------------------------------------------------------------------------
// Synthetic MPEG-TS input
// ---------------------------------------------------------------------------

const INPUT_PMT_PID: u16 = 0x200;
const FRAME_TICKS: u64 = 3_003;

#[derive(Clone, Copy)]
#[allow(clippy::struct_field_names)]
struct InputLayout {
    video_pid: Option<u16>,
    audio_pid: u16,
    pcr_pid: u16,
}

const LAYOUT_A: InputLayout = InputLayout {
    video_pid: Some(0x300),
    audio_pid: 0x301,
    pcr_pid: 0x300,
};

const LAYOUT_B: InputLayout = InputLayout {
    video_pid: Some(0x411),
    audio_pid: 0x412,
    pcr_pid: 0x411,
};

fn input_pat() -> [u8; MPEG_TS_PACKET_SIZE] {
    let mut section = vec![
        0x00,
        0xb0,
        0x0d,
        0x00,
        0x07,
        0xc1,
        0x00,
        0x00,
        0x00,
        0x01,
        0xe0 | (INPUT_PMT_PID >> 8) as u8,
        (INPUT_PMT_PID & 0xff) as u8,
    ];
    append_crc(&mut section);
    input_psi_packet(PAT_PID, &section, 0, 0)
}

fn input_pmt(layout: InputLayout) -> [u8; MPEG_TS_PACKET_SIZE] {
    let mut streams = Vec::new();
    if let Some(video) = layout.video_pid {
        streams.push((0x1b, video, Vec::new()));
    }
    // An ISO 639 language descriptor on the audio stream.
    streams.push((
        0x0f,
        layout.audio_pid,
        vec![0x0a, 0x04, b'e', b'n', b'g', 0x00],
    ));
    // A subtitle stream that the muxer drops.
    streams.push((0x06, 0x0777, vec![0x59, 0x00]));
    let mut section = vec![
        0x02,
        0xb0,
        0x00,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0xe0 | (layout.pcr_pid >> 8) as u8,
        (layout.pcr_pid & 0xff) as u8,
        0xf0,
        0x00,
    ];
    for (stream_type, pid, descriptors) in streams {
        section.extend_from_slice(&[
            stream_type,
            0xe0 | (pid >> 8) as u8,
            (pid & 0xff) as u8,
            0xf0,
            descriptors.len() as u8,
        ]);
        section.extend_from_slice(&descriptors);
    }
    let length = section.len() - 3 + 4;
    section[1] |= (length >> 8) as u8;
    section[2] = (length & 0xff) as u8;
    append_crc(&mut section);
    input_psi_packet(INPUT_PMT_PID, &section, 0, 0)
}

fn encode_timestamp(prefix: u8, value: u64) -> [u8; 5] {
    let mut bytes = [prefix << 4, 0, 0, 0, 0];
    write_timestamp(&mut bytes, value);
    bytes[0] = (bytes[0] & 0x0f) | (prefix << 4);
    bytes
}

/// Builds one packet with an optional PCR, optional random access flag, and a
/// payload. Adaptation stuffing fills the packet.
fn ts_packet(
    pid: u16,
    start: bool,
    counter: u8,
    pcr: Option<u64>,
    random_access: bool,
    payload: &[u8],
) -> [u8; MPEG_TS_PACKET_SIZE] {
    let mut packet = [0xff_u8; MPEG_TS_PACKET_SIZE];
    packet[0] = 0x47;
    packet[1] = (if start { 0x40 } else { 0 }) | (pid >> 8) as u8;
    packet[2] = (pid & 0xff) as u8;
    let adaptation_length = MPEG_TS_PACKET_SIZE - 5 - payload.len();
    packet[3] = 0x30 | (counter & 0x0f);
    packet[4] = adaptation_length as u8;
    if adaptation_length > 0 {
        packet[5] = (if random_access { 0x40 } else { 0 }) | if pcr.is_some() { 0x10 } else { 0 };
        if let Some(pcr) = pcr {
            packet[6] = (pcr >> 25) as u8;
            packet[7] = (pcr >> 17) as u8;
            packet[8] = (pcr >> 9) as u8;
            packet[9] = (pcr >> 1) as u8;
            packet[10] = (((pcr & 1) as u8) << 7) | 0x7e;
            packet[11] = 0;
        }
    }
    packet[5 + adaptation_length..].copy_from_slice(payload);
    packet
}

fn video_pes(pts: u64, dts: u64, keyframe: bool) -> Vec<u8> {
    let mut pes = vec![0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x80, 0xc0, 0x0a];
    pes.extend_from_slice(&encode_timestamp(0x3, pts));
    pes.extend_from_slice(&encode_timestamp(0x1, dts));
    let nal = if keyframe { 0x65 } else { 0x41 };
    pes.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x01, 0x09, 0xf0, 0x00, 0x00, 0x01, nal, 0x88,
    ]);
    pes
}

fn audio_pes(pts: u64) -> Vec<u8> {
    let mut pes = vec![0x00, 0x00, 0x01, 0xc0, 0x00, 0x00, 0x80, 0x80, 0x05];
    pes.extend_from_slice(&encode_timestamp(0x2, pts));
    pes.extend_from_slice(&[0xff, 0xf1, 0x50, 0x80]);
    pes
}

/// Builds `frames` frames of synthetic program data. Frame `i` has PCR
/// `base + i * 3003`, video DTS at the PCR, and PTS 0.1 s later. A PAT and
/// PMT start each group of `gop` frames, and the first frame of each group is
/// a keyframe. `skip` frames at the start are not keyframes.
fn program(layout: InputLayout, base: u64, frames: u64, gop: u64, skip: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut counters: HashMap<u16, u8> = HashMap::new();
    let mut next_cc = |pid: u16| {
        let counter = counters.entry(pid).or_insert(0);
        let value = *counter;
        *counter = (*counter + 1) & 0x0f;
        value
    };
    for frame in 0..frames {
        let dts = (base + frame * FRAME_TICKS) & TIMESTAMP_MASK;
        let pts = (dts + 9_000) & TIMESTAMP_MASK;
        let keyframe = frame >= skip && (frame - skip).is_multiple_of(gop);
        if frame.is_multiple_of(gop) {
            out.extend_from_slice(&input_pat());
            out.extend_from_slice(&input_pmt(layout));
        }
        if let Some(video) = layout.video_pid {
            let pcr = (layout.pcr_pid == video).then_some(dts);
            out.extend_from_slice(&ts_packet(
                video,
                true,
                next_cc(video),
                pcr,
                false,
                &video_pes(pts, dts, keyframe),
            ));
            out.extend_from_slice(&ts_packet(
                video,
                false,
                next_cc(video),
                None,
                false,
                &[0xaa; 100],
            ));
        }
        let pcr = (layout.pcr_pid == layout.audio_pid).then_some(dts);
        out.extend_from_slice(&ts_packet(
            layout.audio_pid,
            true,
            next_cc(layout.audio_pid),
            pcr,
            false,
            &audio_pes(pts),
        ));
        // A packet of the dropped subtitle stream.
        out.extend_from_slice(&ts_packet(0x0777, true, 0, None, false, &[0x20; 20]));
    }
    out
}

// ---------------------------------------------------------------------------
// Output inspection
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct OutPacket {
    pid: u16,
    start: bool,
    counter: u8,
    has_payload: bool,
    discontinuity: bool,
    pcr: Option<u64>,
    pts: Option<u64>,
    dts: Option<u64>,
}

fn parse_output(bytes: &[u8]) -> Vec<OutPacket> {
    assert_eq!(bytes.len() % MPEG_TS_PACKET_SIZE, 0);
    bytes
        .chunks_exact(MPEG_TS_PACKET_SIZE)
        .map(|packet| {
            assert_eq!(packet[0], 0x47);
            let afc = (packet[3] >> 4) & 0x03;
            let discontinuity = afc & 0x02 != 0 && packet[4] > 0 && packet[5] & 0x80 != 0;
            let (pts, dts) = match pes_header(packet) {
                Some((start, _)) if payload_unit_start(packet) => {
                    let flags = packet[start + 7] >> 6;
                    let pts = (flags & 0x02 != 0).then(|| read_timestamp(&packet[start + 9..]));
                    let dts = (flags == 0x03).then(|| read_timestamp(&packet[start + 14..]));
                    (pts, dts)
                }
                _ => (None, None),
            };
            OutPacket {
                pid: packet_pid(packet),
                start: payload_unit_start(packet),
                counter: packet[3] & 0x0f,
                has_payload: afc & 0x01 != 0,
                discontinuity,
                pcr: read_pcr_base(packet),
                pts,
                dts,
            }
        })
        .collect()
}

/// Returns the PSI section in a PAT or PMT packet.
fn section_of(packet: &[u8]) -> &[u8] {
    let pointer = usize::from(packet[4]);
    let section = &packet[5 + pointer..];
    let length = ((usize::from(section[1]) & 0x0f) << 8) | usize::from(section[2]);
    &section[..3 + length]
}

fn packets_of(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.chunks_exact(MPEG_TS_PACKET_SIZE)
}

/// Asserts that values increase, with 33-bit wrap.
fn assert_increasing(label: &str, values: &[u64]) {
    for pair in values.windows(2) {
        let step = pair[1].wrapping_sub(pair[0]) & TIMESTAMP_MASK;
        assert!(
            step > 0 && step < TIMESTAMP_HALF_RANGE,
            "{label} does not increase: {} then {}",
            pair[0],
            pair[1]
        );
    }
}

fn assert_continuity(packets: &[OutPacket]) {
    let mut last: HashMap<u16, u8> = HashMap::new();
    for packet in packets {
        if packet.pid == NULL_PID {
            continue;
        }
        if let Some(previous) = last.get(&packet.pid) {
            let expected = if packet.has_payload {
                (previous + 1) & 0x0f
            } else {
                *previous
            };
            assert_eq!(
                packet.counter, expected,
                "continuity counter gap on PID {:#x}",
                packet.pid
            );
        }
        last.insert(packet.pid, packet.counter);
    }
}

fn splice(first: &[u8], second: &[u8]) -> (SpliceMuxer, BytesMut, usize) {
    let mut muxer = SpliceMuxer::new();
    let mut out = BytesMut::new();
    muxer.begin_pending();
    assert!(muxer.push_pending(first, &mut out));
    let boundary = out.len();
    muxer.begin_pending();
    assert!(muxer.push_pending(second, &mut out));
    (muxer, out, boundary)
}

// ---------------------------------------------------------------------------
// Slate selection and retry rules
// ---------------------------------------------------------------------------

#[test]
fn capacity_errors_select_the_tuners_busy_slate() {
    let error = SessionStartError::Provider(AcquireError::AtCapacity {
        pool_id: "provider".into(),
        capacity: 3,
        active_sessions: 3,
    });
    let failure = StartFailure::classify(&error);
    assert_eq!(failure, StartFailure::Capacity);
    assert_eq!(failure.slate(), SlateKind::TunersBusy);
}

#[test]
fn provider_and_startup_errors_select_the_retrying_slate() {
    for error in [
        SessionStartError::StartupTimeout,
        SessionStartError::Http {
            message: "connection refused".into(),
        },
        SessionStartError::Process {
            adapter: SessionInputAdapter::Ffmpeg,
        },
    ] {
        let failure = StartFailure::classify(&error);
        assert_eq!(failure, StartFailure::Provider, "{error}");
        assert_eq!(failure.slate(), SlateKind::Retrying);
    }
}

#[test]
fn configuration_errors_are_not_retried_at_the_start() {
    for error in [
        SessionStartError::UnknownProvider {
            pool_id: "missing".into(),
        },
        SessionStartError::Provider(AcquireError::LeaseIdExhausted),
        SessionStartError::ProcessInputRequiresProcessAdapter,
    ] {
        assert_eq!(
            StartFailure::classify(&error),
            StartFailure::Configuration,
            "{error}"
        );
    }
    assert_eq!(StartFailure::Configuration.slate(), SlateKind::Retrying);
}

#[test]
fn capacity_retry_keeps_the_fixed_500_ms_interval() {
    let mut retry = RetrySchedule::default();
    for _ in 0..10 {
        assert_eq!(
            retry.next_delay(StartFailure::Capacity),
            Duration::from_millis(500)
        );
    }
    // Capacity retries do not grow the provider back-off.
    assert_eq!(
        retry.next_delay(StartFailure::Provider),
        Duration::from_secs(1)
    );
}

#[test]
fn provider_retry_backs_off_to_30_seconds_and_resets() {
    let mut retry = RetrySchedule::default();
    let delays: Vec<u64> = (0..8)
        .map(|_| retry.next_delay(StartFailure::Provider).as_secs())
        .collect();
    assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
    for _ in 0..100 {
        assert_eq!(retry.next_delay(StartFailure::Provider), PROVIDER_RETRY_MAX);
    }
    retry.reset();
    assert_eq!(
        retry.next_delay(StartFailure::Provider),
        Duration::from_secs(1)
    );
}

// ---------------------------------------------------------------------------
// Readiness and countdown
// ---------------------------------------------------------------------------

fn status(buffered: bool, lead_ms: Option<u64>) -> ViewerStartupStatus {
    ViewerStartupStatus {
        buffered,
        lead: lead_ms.map(Duration::from_millis),
        closed: false,
    }
}

#[test]
fn start_lead_target_is_the_paced_delay_up_to_five_seconds() {
    assert_eq!(start_lead_target(Duration::ZERO), Duration::ZERO);
    assert_eq!(
        start_lead_target(Duration::from_secs(2)),
        Duration::from_secs(2)
    );
    assert_eq!(
        start_lead_target(Duration::from_secs(30)),
        Duration::from_secs(5)
    );
}

#[test]
fn unpaced_viewers_start_at_the_first_media() {
    assert_eq!(
        viewer_readiness(Duration::ZERO, status(true, None), Duration::ZERO),
        Readiness::Ready
    );
    assert!(matches!(
        viewer_readiness(Duration::ZERO, status(false, None), Duration::ZERO),
        Readiness::Waiting { .. }
    ));
}

#[test]
fn paced_viewers_wait_for_the_lead_target() {
    let target = Duration::from_secs(5);
    assert_eq!(
        viewer_readiness(target, status(true, Some(2_000)), Duration::from_secs(2)),
        Readiness::Waiting {
            remaining: Duration::from_secs(3)
        }
    );
    assert_eq!(
        viewer_readiness(target, status(true, Some(5_000)), Duration::from_secs(5)),
        Readiness::Ready
    );
    // A slow upstream that never reaches the target starts after the limit.
    assert_eq!(
        viewer_readiness(target, status(true, Some(1_000)), Duration::from_secs(15)),
        Readiness::Ready
    );
    // An unknown lead starts after the target time.
    assert_eq!(
        viewer_readiness(target, status(true, None), Duration::from_secs(5)),
        Readiness::Ready
    );
}

#[test]
fn viewers_without_media_fail_after_the_limit() {
    let target = Duration::from_secs(5);
    assert!(matches!(
        viewer_readiness(target, status(false, None), Duration::from_secs(29)),
        Readiness::Waiting { .. }
    ));
    assert_eq!(
        viewer_readiness(target, status(false, None), Duration::from_secs(30)),
        Readiness::Failed
    );
    let closed = ViewerStartupStatus {
        buffered: true,
        lead: Some(Duration::from_secs(10)),
        closed: true,
    };
    assert_eq!(
        viewer_readiness(target, closed, Duration::ZERO),
        Readiness::Failed
    );
}

#[test]
fn countdown_rounds_up_and_is_at_least_one_second() {
    assert_eq!(countdown_seconds(Duration::ZERO), 1);
    assert_eq!(countdown_seconds(Duration::from_millis(1)), 1);
    assert_eq!(countdown_seconds(Duration::from_millis(2_001)), 3);
    assert_eq!(countdown_seconds(Duration::from_secs(5)), 5);
}

// ---------------------------------------------------------------------------
// Slate profile and FFmpeg arguments
// ---------------------------------------------------------------------------

#[test]
fn missing_profile_data_gives_the_safe_default() {
    let profile = SlateProfile::from_measured(&MeasuredStreamProfile::default());
    assert_eq!(profile, SlateProfile::default());
    assert_eq!(profile.video, SlateVideoCodec::H264);
    assert_eq!((profile.width, profile.height), (1_280, 720));
    assert_eq!(profile.frame_rate, FrameRate::new(30, 1));
    assert_eq!(profile.audio, SlateAudioCodec::Aac);
    assert_eq!(profile.audio_channels, 2);
    assert_eq!(profile.sample_rate, 48_000);
}

#[test]
fn measured_profile_controls_the_slate_encode() {
    let profile = SlateProfile::from_measured(&MeasuredStreamProfile {
        video_codec: Some("h265".into()),
        video_width: Some(1_921),
        video_height: Some(1_081),
        video_fps: Some(29.97),
        audio_codec: Some("eac3".into()),
        audio_channels: Some(6),
        audio_sample_rate: Some(44_100),
    });
    assert_eq!(profile.video, SlateVideoCodec::H265);
    assert_eq!((profile.width, profile.height), (1_920, 1_080));
    assert_eq!(profile.frame_rate, FrameRate::new(30_000, 1_001));
    assert_eq!(profile.audio, SlateAudioCodec::Eac3);
    assert_eq!(profile.audio_channels, 6);
    assert_eq!(profile.sample_rate, 44_100);
}

#[test]
fn unsupported_profile_values_fall_back_one_by_one() {
    let profile = SlateProfile::from_measured(&MeasuredStreamProfile {
        video_codec: Some("mpeg2-video".into()),
        video_width: Some(10_000),
        video_height: Some(720),
        video_fps: Some(17.0),
        audio_codec: Some("mp2".into()),
        audio_channels: Some(6),
        audio_sample_rate: Some(22_050),
    });
    assert_eq!(profile.video, SlateVideoCodec::Mpeg2);
    assert_eq!((profile.width, profile.height), (1_280, 720));
    assert_eq!(profile.frame_rate, FrameRate::new(30, 1));
    assert_eq!(profile.audio, SlateAudioCodec::Mp2);
    // MP2 has no 5.1 layout in the slate encoder.
    assert_eq!(profile.audio_channels, 2);
    assert_eq!(profile.sample_rate, 48_000);
    assert_eq!(nearest_frame_rate(25.0), Some(FrameRate::new(25, 1)));
    assert_eq!(
        nearest_frame_rate(59.94),
        Some(FrameRate::new(60_000, 1_001))
    );
    assert_eq!(nearest_frame_rate(f64::NAN), None);
}

fn argument_strings(kind: SlateKind, countdown: u32, profile: &SlateProfile) -> Vec<String> {
    ffmpeg_arguments(kind, countdown, profile)
        .into_iter()
        .map(|argument| argument.into_string().unwrap())
        .collect()
}

fn value_after(arguments: &[String], flag: &str) -> String {
    let index = arguments
        .iter()
        .position(|argument| argument == flag)
        .unwrap();
    arguments[index + 1].clone()
}

#[test]
fn starting_slate_has_an_eif_countdown() {
    let arguments = argument_strings(SlateKind::Starting, 5, &SlateProfile::default());
    let filter = value_after(&arguments, "-vf");
    assert!(filter.starts_with("drawtext=text='Stream starting in %{eif"));
    assert!(filter.contains("ceil(5-t)"));
    assert!(filter.contains("seconds'"));
}

#[test]
fn busy_and_retry_slates_show_the_required_messages() {
    let busy = value_after(
        &argument_strings(SlateKind::TunersBusy, 1, &SlateProfile::default()),
        "-vf",
    );
    assert!(busy.contains("All tuners in use,"));
    assert!(busy.contains("stay tuned for one to open up"));
    let retry = value_after(
        &argument_strings(SlateKind::Retrying, 1, &SlateProfile::default()),
        "-vf",
    );
    assert!(retry.contains("Having trouble reaching this channel,"));
    assert!(retry.contains("retrying"));
}

#[test]
fn slate_arguments_follow_the_profile_and_fixed_pids() {
    let profile = SlateProfile {
        video: SlateVideoCodec::Mpeg2,
        width: 720,
        height: 576,
        frame_rate: FrameRate::new(25, 1),
        audio: SlateAudioCodec::Ac3,
        audio_channels: 6,
        sample_rate: 48_000,
    };
    let arguments = argument_strings(SlateKind::Retrying, 1, &profile);
    assert_eq!(arguments.first().map(String::as_str), Some("-hide_banner"));
    assert_eq!(arguments.last().map(String::as_str), Some("pipe:1"));
    assert!(arguments.contains(&"color=c=0x08111f:s=720x576:r=25/1".to_owned()));
    assert!(arguments.contains(&"anullsrc=channel_layout=5.1:sample_rate=48000".to_owned()));
    assert_eq!(value_after(&arguments, "-c:v"), "mpeg2video");
    assert_eq!(value_after(&arguments, "-c:a"), "ac3");
    assert_eq!(value_after(&arguments, "-b:a"), "448k");
    assert_eq!(value_after(&arguments, "-g"), "25");
    assert_eq!(value_after(&arguments, "-mpegts_start_pid"), "256");
    assert_eq!(value_after(&arguments, "-mpegts_pmt_start_pid"), "4096");
    assert!(arguments.iter().all(|argument| !argument.contains("://")));
}

#[test]
fn slate_command_uses_the_fixed_ffmpeg_executable() {
    let command =
        FfmpegInputAdapter::new().slate_command(SlateKind::TunersBusy, 1, &SlateProfile::default());
    assert_eq!(command.executable(), std::ffi::OsStr::new("ffmpeg"));
    assert_eq!(
        command.arguments(),
        ffmpeg_arguments(SlateKind::TunersBusy, 1, &SlateProfile::default()).as_slice()
    );
}

// ---------------------------------------------------------------------------
// Splice packet rules
// ---------------------------------------------------------------------------

#[test]
fn first_segment_starts_with_pat_and_pmt_and_no_discontinuity() {
    let mut muxer = SpliceMuxer::new();
    let mut out = BytesMut::new();
    muxer.begin_pending();
    assert!(muxer.push_pending(&program(LAYOUT_A, 900_000, 30, 15, 0), &mut out));
    let packets = parse_output(&out);
    assert_eq!(packets[0].pid, PAT_PID);
    assert_eq!(packets[1].pid, SLATE_PMT_PID);
    assert!(packets.iter().all(|packet| !packet.discontinuity));
    assert_eq!(muxer.splices(), 1);
    // The first segment keeps its own timestamps.
    let first_pcr = packets.iter().find_map(|packet| packet.pcr).unwrap();
    assert_eq!(first_pcr, 900_000);
}

#[test]
fn output_uses_only_the_fixed_pids() {
    let (_, out, _) = splice(
        &program(LAYOUT_A, 900_000, 30, 15, 0),
        &program(LAYOUT_B, 50_000, 30, 15, 0),
    );
    let packets = parse_output(&out);
    for packet in &packets {
        assert!(
            matches!(
                packet.pid,
                PAT_PID | SLATE_PMT_PID | SLATE_VIDEO_PID | SLATE_AUDIO_PID
            ),
            "unexpected PID {:#x}",
            packet.pid
        );
    }
    assert!(packets.iter().any(|packet| packet.pid == SLATE_VIDEO_PID));
    assert!(packets.iter().any(|packet| packet.pid == SLATE_AUDIO_PID));
}

#[test]
fn output_pat_and_pmt_are_valid_and_describe_the_fixed_layout() {
    let (_, out, _) = splice(
        &program(LAYOUT_A, 900_000, 30, 15, 0),
        &program(LAYOUT_B, 50_000, 30, 15, 0),
    );
    let mut versions = Vec::new();
    for packet in packets_of(&out) {
        let pid = packet_pid(packet);
        if pid != PAT_PID && pid != SLATE_PMT_PID {
            continue;
        }
        let section = section_of(packet);
        assert_eq!(mpeg_crc32(section), 0, "PSI CRC on PID {pid:#x}");
        if pid == PAT_PID {
            assert_eq!(&section[8..12], &[0x00, 0x01, 0xf0, 0x00]);
            continue;
        }
        let map = ProgramMap::from_pmt(section).unwrap();
        assert_eq!(map.pcr_pid, Some(SLATE_VIDEO_PID));
        let video = map.video.unwrap();
        assert_eq!((video.pid, video.stream_type), (SLATE_VIDEO_PID, 0x1b));
        let audio = map.audio.unwrap();
        assert_eq!((audio.pid, audio.stream_type), (SLATE_AUDIO_PID, 0x0f));
        // The audio language descriptor stays.
        assert_eq!(audio.descriptors, [0x0a, 0x04, b'e', b'n', b'g', 0x00]);
        versions.push((section[5] >> 1) & 0x1f);
    }
    versions.dedup();
    assert_eq!(versions, [1, 2], "each segment has a new PMT version");
}

#[test]
fn splice_sends_pat_pmt_then_discontinuity_markers() {
    let (_, out, boundary) = splice(
        &program(LAYOUT_A, 900_000, 30, 15, 0),
        &program(LAYOUT_B, 50_000, 30, 15, 0),
    );
    let after = parse_output(&out[boundary..]);
    assert_eq!(after[0].pid, PAT_PID);
    assert_eq!(after[1].pid, SLATE_PMT_PID);
    let markers: Vec<&OutPacket> = after[2..4].iter().collect();
    let mut marker_pids: Vec<u16> = markers.iter().map(|packet| packet.pid).collect();
    marker_pids.sort_unstable();
    assert_eq!(marker_pids, [SLATE_VIDEO_PID, SLATE_AUDIO_PID]);
    for marker in markers {
        assert!(marker.discontinuity);
        assert!(!marker.has_payload);
    }
    // The first media packet after the markers is a video keyframe start.
    assert_eq!(after[4].pid, SLATE_VIDEO_PID);
    assert!(after[4].start);
    assert!(after[4].pcr.is_some());
}

#[test]
fn splice_keeps_pcr_pts_and_dts_monotonic_when_the_input_goes_back() {
    let (_, out, boundary) = splice(
        &program(LAYOUT_A, 900_000, 30, 15, 0),
        &program(LAYOUT_B, 50_000, 30, 15, 0),
    );
    let packets = parse_output(&out);
    let pcrs: Vec<u64> = packets.iter().filter_map(|packet| packet.pcr).collect();
    assert_increasing("PCR", &pcrs);
    for pid in [SLATE_VIDEO_PID, SLATE_AUDIO_PID] {
        let pts: Vec<u64> = packets
            .iter()
            .filter(|packet| packet.pid == pid)
            .filter_map(|packet| packet.pts)
            .collect();
        assert_increasing("PTS", &pts);
    }
    let dts: Vec<u64> = packets.iter().filter_map(|packet| packet.dts).collect();
    assert_increasing("DTS", &dts);

    // The last PES of each stream lasts one frame. The first spliced
    // timestamp comes one frame after that end.
    let before = parse_output(&out[..boundary]);
    let last_pts = before.iter().filter_map(|packet| packet.pts).max().unwrap();
    let first_new_pcr = parse_output(&out[boundary..])
        .iter()
        .find_map(|packet| packet.pcr)
        .unwrap();
    assert_eq!(first_new_pcr, last_pts + FRAME_TICKS + SPLICE_STEP_TICKS);
}

#[test]
fn splice_stays_monotonic_across_the_33_bit_wrap() {
    let near_wrap = TIMESTAMP_MASK - 20 * FRAME_TICKS;
    let (_, out, _) = splice(
        &program(LAYOUT_A, near_wrap, 30, 15, 0),
        &program(LAYOUT_B, 123_456, 30, 15, 0),
    );
    let packets = parse_output(&out);
    let pcrs: Vec<u64> = packets.iter().filter_map(|packet| packet.pcr).collect();
    assert!(pcrs.iter().any(|pcr| *pcr < near_wrap), "the PCR wraps");
    assert_increasing("PCR", &pcrs);
    let pts: Vec<u64> = packets
        .iter()
        .filter(|packet| packet.pid == SLATE_VIDEO_PID)
        .filter_map(|packet| packet.pts)
        .collect();
    assert_increasing("PTS", &pts);
}

#[test]
fn continuity_counters_continue_across_the_splice() {
    let (_, out, _) = splice(
        &program(LAYOUT_A, 900_000, 37, 15, 0),
        &program(LAYOUT_B, 50_000, 30, 15, 0),
    );
    assert_continuity(&parse_output(&out));
}

#[test]
fn pending_segment_waits_for_a_keyframe() {
    let mut muxer = SpliceMuxer::new();
    let mut out = BytesMut::new();
    muxer.begin_pending();
    assert!(muxer.push_pending(&program(LAYOUT_A, 900_000, 30, 15, 0), &mut out));
    muxer.begin_pending();
    let second = program(LAYOUT_B, 50_000, 30, 30, 10);
    let split = second
        .chunks_exact(MPEG_TS_PACKET_SIZE)
        .position(|packet| {
            packet_pid(packet) == 0x411 && payload_unit_start(packet) && is_keyframe(packet, 0x1b)
        })
        .unwrap()
        * MPEG_TS_PACKET_SIZE;
    let before = out.len();
    assert!(!muxer.push_pending(&second[..split], &mut out));
    assert_eq!(out.len(), before, "no packets before the keyframe");
    assert!(muxer.has_pending());
    assert!(muxer.push_pending(&second[split..], &mut out));
    assert!(!muxer.has_pending());
    let after = parse_output(&out[before..]);
    let first_media = after
        .iter()
        .find(|packet| packet.pid == SLATE_VIDEO_PID && packet.has_payload)
        .unwrap();
    assert!(first_media.start);
}

#[test]
fn current_segment_continues_until_the_pending_segment_starts() {
    let mut muxer = SpliceMuxer::new();
    let mut out = BytesMut::new();
    muxer.begin_pending();
    let slate = program(LAYOUT_A, 900_000, 60, 15, 0);
    let (first, rest) = slate.split_at(slate.len() / 2);
    assert!(muxer.push_pending(first, &mut out));
    muxer.begin_pending();
    // The live input has no keyframe yet, so the slate continues.
    let live = program(LAYOUT_B, 50_000, 30, 30, 10);
    let split = 10 * 4 * MPEG_TS_PACKET_SIZE;
    assert!(!muxer.push_pending(&live[..split], &mut out));
    let before = out.len();
    muxer.push_current(rest, &mut out);
    assert!(out.len() > before);
    assert!(muxer.push_pending(&live[split..], &mut out));
    let packets = parse_output(&out);
    assert_increasing(
        "PCR",
        &packets
            .iter()
            .filter_map(|packet| packet.pcr)
            .collect::<Vec<_>>(),
    );
    assert_continuity(&packets);
}

#[test]
fn audio_only_programs_start_at_an_audio_pes() {
    let layout = InputLayout {
        video_pid: None,
        audio_pid: 0x301,
        pcr_pid: 0x301,
    };
    let mut muxer = SpliceMuxer::new();
    let mut out = BytesMut::new();
    muxer.begin_pending();
    assert!(muxer.push_pending(&program(layout, 900_000, 10, 5, 0), &mut out));
    let packets = parse_output(&out);
    let map = ProgramMap::from_pmt(section_of(&out[MPEG_TS_PACKET_SIZE..])).unwrap();
    assert!(map.video.is_none());
    assert_eq!(map.pcr_pid, Some(SLATE_AUDIO_PID));
    assert!(packets.iter().any(|packet| packet.pid == SLATE_AUDIO_PID));
}

#[test]
fn input_pat_repeats_become_output_pat_and_pmt() {
    let mut muxer = SpliceMuxer::new();
    let mut out = BytesMut::new();
    muxer.begin_pending();
    assert!(muxer.push_pending(&program(LAYOUT_A, 900_000, 45, 15, 0), &mut out));
    let packets = parse_output(&out);
    let pat_packets = packets
        .iter()
        .filter(|packet| packet.pid == PAT_PID)
        .count();
    let program_map_packets = packets
        .iter()
        .filter(|packet| packet.pid == SLATE_PMT_PID)
        .count();
    assert_eq!(pat_packets, 3);
    assert_eq!(program_map_packets, 3);
}

#[test]
fn fixture_splices_to_itself_with_valid_packet_rules() {
    let fixture: &[u8] = include_bytes!("../../../../tests/fixtures/ts/h264_720p_aac.ts");
    let (_, out, boundary) = splice(fixture, fixture);
    let packets = parse_output(&out);
    assert_continuity(&packets);
    let pcrs: Vec<u64> = packets.iter().filter_map(|packet| packet.pcr).collect();
    assert!(pcrs.len() > 4);
    assert_increasing("PCR", &pcrs);
    for pid in [SLATE_VIDEO_PID, SLATE_AUDIO_PID] {
        let dts_or_pts: Vec<u64> = packets
            .iter()
            .filter(|packet| packet.pid == pid)
            .filter_map(|packet| packet.dts.or(packet.pts))
            .collect();
        assert!(dts_or_pts.len() > 2, "PID {pid:#x} has media");
        assert_increasing("decode time", &dts_or_pts);
    }
    let after = parse_output(&out[boundary..]);
    assert_eq!(after[0].pid, PAT_PID);
    assert_eq!(after[1].pid, SLATE_PMT_PID);
    assert!(after[2].discontinuity);
}
