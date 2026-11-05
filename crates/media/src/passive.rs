//! Passive stream metadata capture from MPEG-TS data that a session already
//! holds.
//!
//! The analyzer reads the PAT and PMT, then scans the video and audio
//! elementary streams for an H.264 sequence parameter set and an AAC ADTS
//! header. It never opens a network connection. A caller feeds it packets from
//! an independent ring cursor of a live session.

use std::time::Duration;

use bytes::Bytes;

use crate::{
    MPEG_TS_PACKET_SIZE, RingCursor, RingRead,
    probe::codec_name_for_stream_type,
    psi::{PatPmtTracker, parse_pmt_elementary_streams},
};

/// The maximum elementary stream bytes kept for each of video and audio.
const MAX_ES_BYTES: usize = 192 * 1024;
/// The maximum packets one ring read returns during sampling.
const SAMPLE_READ_PACKETS: usize = 128;

/// Stream metadata that the analyzer measured from transport stream data.
///
/// Each field is `None` when the data did not contain it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamMetadata {
    pub video_codec: Option<String>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub video_fps: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<u8>,
    pub audio_sample_rate: Option<u32>,
}

impl StreamMetadata {
    /// Returns true when the video size and the audio codec are known.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.video_width.is_some() && self.video_height.is_some() && self.audio_codec.is_some()
    }

    /// Returns true when the metadata has at least a video codec.
    #[must_use]
    pub fn has_video(&self) -> bool {
        self.video_codec.is_some()
    }
}

/// Incremental analyzer for MPEG-TS packets.
#[derive(Debug, Default)]
pub struct PassiveStreamAnalyzer {
    tracker: PatPmtTracker,
    sequence: u64,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    video_type: Option<u8>,
    audio_type: Option<u8>,
    video_es: Vec<u8>,
    audio_es: Vec<u8>,
    metadata: StreamMetadata,
}

impl PassiveStreamAnalyzer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds whole 188-byte packets. Bytes that do not fill a packet are
    /// ignored.
    pub fn push(&mut self, bytes: &[u8]) {
        for packet in bytes.chunks_exact(MPEG_TS_PACKET_SIZE) {
            self.sequence += 1;
            self.observe_packet(packet);
        }
    }

    /// Returns the metadata measured so far.
    #[must_use]
    pub fn metadata(&self) -> &StreamMetadata {
        &self.metadata
    }

    /// Returns true when no more data can improve the result.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.metadata.is_complete()
    }

    fn observe_packet(&mut self, packet: &[u8]) {
        if packet[0] != 0x47 {
            return;
        }
        if self.tracker.observe(packet, self.sequence).is_some() {
            self.apply_pmt();
        }
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        let is_video = Some(pid) == self.video_pid;
        let is_audio = Some(pid) == self.audio_pid;
        if !is_video && !is_audio {
            return;
        }
        let Some(payload) = packet_payload(packet) else {
            return;
        };
        if is_video && self.metadata.video_width.is_none() && self.video_es.len() < MAX_ES_BYTES {
            self.video_es.extend_from_slice(payload);
            if self.video_type == Some(0x1b)
                && let Some(sps) = find_h264_sps(&self.video_es)
            {
                self.metadata.video_width = Some(sps.width);
                self.metadata.video_height = Some(sps.height);
                self.metadata.video_fps = sps.fps;
                self.video_es = Vec::new();
            }
        }
        if is_audio
            && self.metadata.audio_sample_rate.is_none()
            && self.audio_es.len() < MAX_ES_BYTES
        {
            self.audio_es.extend_from_slice(payload);
            if matches!(self.audio_type, Some(0x0f))
                && let Some((rate, channels)) = find_adts_header(&self.audio_es)
            {
                self.metadata.audio_sample_rate = Some(rate);
                self.metadata.audio_channels = Some(channels);
                self.audio_es = Vec::new();
            }
        }
    }

    fn apply_pmt(&mut self) {
        let Some((section, program)) = self.tracker.completed_pmt() else {
            return;
        };
        let Some(streams) = parse_pmt_elementary_streams(section, program) else {
            return;
        };
        let mut video = None;
        let mut audio = None;
        for entry in &streams {
            match entry.stream_type {
                0x01 | 0x02 | 0x10 | 0x1b | 0x24 => {
                    video.get_or_insert((entry.pid, entry.stream_type));
                }
                0x03 | 0x04 | 0x0f | 0x11 | 0x80 | 0x81 | 0x82 | 0x83 | 0x84 | 0x87 => {
                    audio.get_or_insert((entry.pid, entry.stream_type));
                }
                _ => {}
            }
        }
        if let Some((pid, stream_type)) = video
            && self.video_pid != Some(pid)
        {
            self.video_pid = Some(pid);
            self.video_type = Some(stream_type);
            self.video_es.clear();
            self.metadata.video_codec = codec_name_for_stream_type(stream_type).map(str::to_owned);
        }
        if let Some((pid, stream_type)) = audio
            && self.audio_pid != Some(pid)
        {
            self.audio_pid = Some(pid);
            self.audio_type = Some(stream_type);
            self.audio_es.clear();
            self.metadata.audio_codec = codec_name_for_stream_type(stream_type).map(str::to_owned);
        }
    }
}

/// Reads packets from an independent ring cursor until the metadata is
/// complete or the time limit ends.
///
/// The function reads from the shared ring buffer only. It opens no provider
/// connection and does not change the state of the live session.
pub async fn sample_cursor_metadata(
    mut cursor: RingCursor,
    max_duration: Duration,
) -> Option<StreamMetadata> {
    let mut analyzer = PassiveStreamAnalyzer::new();
    let deadline = tokio::time::Instant::now() + max_duration;
    loop {
        match tokio::time::timeout_at(deadline, cursor.next(SAMPLE_READ_PACKETS)).await {
            Ok(Ok(RingRead::Packets { bytes, .. })) => {
                let bytes: Bytes = bytes;
                analyzer.push(&bytes);
                if analyzer.is_done() {
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let metadata = analyzer.metadata().clone();
    metadata.has_video().then_some(metadata)
}

/// Returns the payload slice of one transport packet.
fn packet_payload(packet: &[u8]) -> Option<&[u8]> {
    let adaptation_control = (packet[3] >> 4) & 0x03;
    let offset = match adaptation_control {
        1 => 4,
        3 => 5 + usize::from(packet[4]),
        _ => return None,
    };
    packet.get(offset..)
}

struct SpsInfo {
    width: u32,
    height: u32,
    fps: Option<f64>,
}

/// Finds the first parsable H.264 SPS in an Annex B byte sequence.
fn find_h264_sps(data: &[u8]) -> Option<SpsInfo> {
    let mut index = 0;
    while index + 4 < data.len() {
        if data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1 {
            if data[index + 3] & 0x1f == 7 {
                let start = index + 4;
                let mut end = data.len();
                let mut scan = start;
                while scan + 2 < data.len() {
                    if data[scan] == 0 && data[scan + 1] == 0 && data[scan + 2] <= 1 {
                        end = scan;
                        break;
                    }
                    scan += 1;
                }
                // A NAL that reaches the end of the buffer can be partial.
                if end < data.len()
                    && let Some(info) = parse_sps(&unescape(&data[start..end]))
                {
                    return Some(info);
                }
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    None
}

/// Removes emulation prevention bytes from a NAL payload.
fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &byte in data {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.position / 8)?;
        let bit = (byte >> (7 - (self.position % 8))) & 1;
        self.position += 1;
        Some(u32::from(bit))
    }

    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0_u32;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Some(value)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        Some((1_u32 << zeros) - 1 + rest)
    }

    fn se(&mut self) -> Option<i32> {
        let value = self.ue()?;
        let magnitude = i32::try_from(value.div_ceil(2)).ok()?;
        Some(if value % 2 == 1 {
            magnitude
        } else {
            -magnitude
        })
    }

    fn skip_scaling_list(&mut self, size: usize) -> Option<()> {
        let mut last = 8_i32;
        let mut next = 8_i32;
        for _ in 0..size {
            if next != 0 {
                next = (last + self.se()? + 256).rem_euclid(256);
            }
            if next != 0 {
                last = next;
            }
        }
        Some(())
    }
}

/// Parses an unescaped H.264 SPS payload that follows the NAL header byte.
fn parse_sps(rbsp: &[u8]) -> Option<SpsInfo> {
    let mut reader = BitReader::new(rbsp);
    let profile = reader.bits(8)?;
    reader.bits(16)?; // constraint flags and level
    reader.ue()?; // sps id
    let mut chroma_format = 1;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format = reader.ue()?;
        if chroma_format == 3 {
            reader.bit()?;
        }
        reader.ue()?;
        reader.ue()?;
        reader.bit()?;
        if reader.bit()? == 1 {
            let lists = if chroma_format == 3 { 12 } else { 8 };
            for index in 0..lists {
                if reader.bit()? == 1 {
                    reader.skip_scaling_list(if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    reader.ue()?; // log2_max_frame_num_minus4
    match reader.ue()? {
        0 => {
            reader.ue()?;
        }
        1 => {
            reader.bit()?;
            reader.se()?;
            reader.se()?;
            for _ in 0..reader.ue()? {
                reader.se()?;
            }
        }
        _ => {}
    }
    reader.ue()?; // max_num_ref_frames
    reader.bit()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = reader.ue()?.checked_add(1)?;
    let height_map_units = reader.ue()?.checked_add(1)?;
    let frame_mbs_only = reader.bit()?;
    if frame_mbs_only == 0 {
        reader.bit()?;
    }
    reader.bit()?; // direct_8x8_inference_flag
    let (mut crop_left, mut crop_right, mut crop_top, mut crop_bottom) = (0, 0, 0, 0);
    if reader.bit()? == 1 {
        crop_left = reader.ue()?;
        crop_right = reader.ue()?;
        crop_top = reader.ue()?;
        crop_bottom = reader.ue()?;
    }
    let (unit_x, unit_y) = match chroma_format {
        0 | 3 => (1, 2 - frame_mbs_only),
        _ => (2, 2 * (2 - frame_mbs_only)),
    };
    let width = (width_mbs * 16).checked_sub((crop_left + crop_right) * unit_x)?;
    let height = ((2 - frame_mbs_only) * height_map_units * 16)
        .checked_sub((crop_top + crop_bottom) * unit_y)?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return None;
    }
    let fps = parse_vui_fps(&mut reader);
    Some(SpsInfo { width, height, fps })
}

/// Reads the VUI timing fields. Returns `None` when the VUI is absent or
/// truncated.
fn parse_vui_fps(reader: &mut BitReader<'_>) -> Option<f64> {
    if reader.bit()? == 0 {
        return None;
    }
    if reader.bit()? == 1 && reader.bits(8)? == 255 {
        reader.bits(32)?;
    }
    if reader.bit()? == 1 {
        reader.bit()?;
    }
    if reader.bit()? == 1 {
        reader.bits(4)?;
        if reader.bit()? == 1 {
            reader.bits(24)?;
        }
    }
    if reader.bit()? == 1 {
        reader.ue()?;
        reader.ue()?;
    }
    if reader.bit()? == 0 {
        return None;
    }
    let units = reader.bits(32)?;
    let scale = reader.bits(32)?;
    if units == 0 || scale == 0 {
        return None;
    }
    let fps = f64::from(scale) / (2.0 * f64::from(units));
    if !(1.0..=240.0).contains(&fps) {
        return None;
    }
    Some((fps * 100.0).round() / 100.0)
}

/// Finds the first ADTS header and returns the sample rate and channel count.
fn find_adts_header(data: &[u8]) -> Option<(u32, u8)> {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    for window in data.windows(7) {
        if window[0] == 0xff && window[1] & 0xf6 == 0xf0 {
            let rate_index = usize::from((window[2] >> 2) & 0x0f);
            let channel_config = ((window[2] & 1) << 2) | (window[3] >> 6);
            let frame_length = (usize::from(window[3] & 3) << 11)
                | (usize::from(window[4]) << 3)
                | usize::from(window[5] >> 5);
            if let Some(&rate) = RATES.get(rate_index)
                && frame_length >= 7
                && (1..=7).contains(&channel_config)
            {
                let channels = if channel_config == 7 {
                    8
                } else {
                    channel_config
                };
                return Some((rate, channels));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/ts/h264_720p_aac.ts");

    #[test]
    fn fixture_yields_720p_h264_and_aac_values() {
        let mut analyzer = PassiveStreamAnalyzer::new();
        analyzer.push(FIXTURE);
        let metadata = analyzer.metadata();
        assert_eq!(metadata.video_codec.as_deref(), Some("h264"));
        assert_eq!(metadata.video_width, Some(1280));
        assert_eq!(metadata.video_height, Some(720));
        assert_eq!(metadata.video_fps, Some(29.97));
        assert_eq!(metadata.audio_codec.as_deref(), Some("aac"));
        assert_eq!(metadata.audio_channels, Some(2));
        assert_eq!(metadata.audio_sample_rate, Some(48_000));
        assert!(analyzer.is_done());
    }

    #[test]
    fn fixture_fed_in_small_odd_chunks_gives_the_same_values() {
        let mut analyzer = PassiveStreamAnalyzer::new();
        for chunk in FIXTURE.chunks(MPEG_TS_PACKET_SIZE * 3) {
            analyzer.push(chunk);
        }
        assert_eq!(analyzer.metadata().video_width, Some(1280));
        assert_eq!(analyzer.metadata().video_height, Some(720));
    }

    #[test]
    fn empty_and_garbage_input_yield_no_metadata() {
        let mut analyzer = PassiveStreamAnalyzer::new();
        analyzer.push(&[]);
        analyzer.push(&[0xaa; MPEG_TS_PACKET_SIZE * 4]);
        assert_eq!(analyzer.metadata(), &StreamMetadata::default());
        assert!(!analyzer.is_done());
    }

    fn ue(push: &mut impl FnMut(u32, u32), value: u32) {
        let coded = value + 1;
        let length = 32 - coded.leading_zeros();
        push(0, length - 1);
        push(coded, length);
    }

    #[test]
    fn sps_with_cropping_reports_1080p() {
        // Baseline-profile style SPS for 1920x1088 with 8 rows cropped.
        let mut bits: Vec<u8> = Vec::new();
        let mut push = |value: u32, count: u32| {
            for shift in (0..count).rev() {
                bits.push(u8::try_from((value >> shift) & 1).unwrap());
            }
        };
        push(66, 8);
        push(0, 8);
        push(40, 8);
        ue(&mut push, 0); // sps id
        ue(&mut push, 0); // log2_max_frame_num
        ue(&mut push, 2); // poc type 2
        ue(&mut push, 1); // max refs
        push(0, 1);
        ue(&mut push, 119); // 120 macroblocks wide
        ue(&mut push, 67); // 68 macroblocks high
        push(1, 1); // frame_mbs_only
        push(1, 1); // direct 8x8
        push(1, 1); // cropping
        ue(&mut push, 0);
        ue(&mut push, 0);
        ue(&mut push, 0);
        ue(&mut push, 4); // bottom crop 4 * 2 = 8 rows
        push(0, 1); // no VUI
        while !bits.len().is_multiple_of(8) {
            bits.push(0);
        }
        let bytes: Vec<u8> = bits
            .chunks(8)
            .map(|byte| byte.iter().fold(0_u8, |acc, bit| (acc << 1) | bit))
            .collect();
        let info = parse_sps(&bytes).expect("SPS parses");
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.fps, None);
    }
}
