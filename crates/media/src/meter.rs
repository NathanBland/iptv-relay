//! Measured stream rates for one ring generation.
//!
//! The ring feeds every packet through a [`RateMeter`] while it holds the ring
//! lock. The meter measures from the stream's own clocks, not from the network
//! delivery rate:
//!
//! - **Bitrate** is bytes for each second of PCR media time. A provider burst
//!   delivers media faster than real time, but each media second keeps its
//!   real size, so bursts do not inflate the bitrate.
//! - **Burst** is the largest swing between media time and wall-clock time.
//!   A provider that sends 15 s of media at connect time has a burst of 15 s.
//! - **Frame rate** is the median spacing of the video PES decode timestamps.
//!   It does not use codec header fields, so it works for MPEG-2, H.264 and
//!   HEVC.

use std::{collections::VecDeque, time::Duration};

use tokio::time::Instant;

use crate::{pacing::PCR_TICKS_PER_SECOND, ring::MPEG_TS_PACKET_SIZE};

/// Media seconds kept for the bitrate calculation.
const WINDOW_SECONDS: usize = 120;
/// Media seconds in one peak window. A short window catches single large
/// frames; this window catches sustained high-motion scenes, which is what
/// fills a ring.
const PEAK_WINDOW_SECONDS: usize = 5;
/// Complete media seconds required before the meter reports a bitrate.
const MIN_COMPLETE_SECONDS: usize = 8;
/// Decode-timestamp gaps kept for the frame rate calculation.
const FRAME_SAMPLES: usize = 120;
/// Gaps required before the meter reports a frame rate.
const MIN_FRAME_SAMPLES: usize = 30;
/// PES timestamps are 33-bit values at 90 kHz.
const PTS_WRAP: u64 = 1 << 33;
const PTS_TICKS_PER_SECOND: u64 = 90_000;
/// Standard frame rates in millihertz. A measured rate within 0.5 % of one
/// of them is reported as that rate.
const STANDARD_RATES_MILLIHERTZ: [u32; 8] = [
    23_976, 24_000, 25_000, 29_970, 30_000, 50_000, 59_940, 60_000,
];

/// Rates measured for one ring generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamRates {
    /// Mean bitrate over the measured window, in bits per media second.
    pub average_bits_per_second: u64,
    /// Highest mean bitrate over any 5 s of media in the window.
    pub peak_bits_per_second: u64,
    /// Largest swing between media time and wall-clock time. This is how far
    /// ahead of real time the provider has delivered media.
    pub burst: Duration,
    /// Video frame rate in millihertz (59 940 is 59.94 fps). `None` until
    /// enough video timestamps are seen. Field-coded video that carries one
    /// PES for each field reports the field rate.
    pub frame_rate_millihertz: Option<u32>,
    /// Media time covered by the measurement.
    pub measured: Duration,
}

#[derive(Clone, Copy, Debug)]
struct ClockMark {
    ticks: u64,
    sequence: u64,
}

#[derive(Debug, Default)]
pub(crate) struct RateMeter {
    last: Option<ClockMark>,
    origin: Option<(u64, Instant)>,
    /// Packets for each media second. The back entry is the current, still
    /// incomplete second.
    seconds: VecDeque<u64>,
    current_second: u64,
    measured_ticks: u64,
    min_offset_millis: i64,
    max_offset_millis: i64,
    video_pid: Option<u16>,
    last_decode: Option<u64>,
    frame_gaps: VecDeque<u32>,
}

impl RateMeter {
    pub(crate) fn video_pid(&self) -> Option<u16> {
        self.video_pid
    }

    pub(crate) fn set_video_pid(&mut self, pid: Option<u16>) {
        if self.video_pid != pid {
            self.video_pid = pid;
            self.last_decode = None;
            self.frame_gaps.clear();
        }
    }

    /// Records the PCR timeline position `ticks` at packet `sequence`,
    /// received at `now`.
    pub(crate) fn observe_clock(&mut self, ticks: u64, sequence: u64, now: Instant) {
        let Some(last) = self.last else {
            self.last = Some(ClockMark { ticks, sequence });
            self.origin = Some((ticks, now));
            self.current_second = ticks / PCR_TICKS_PER_SECOND;
            self.seconds.push_back(0);
            return;
        };
        if sequence <= last.sequence {
            return;
        }
        let packets = sequence - last.sequence;
        let elapsed_ticks = ticks.saturating_sub(last.ticks);
        let second = ticks / PCR_TICKS_PER_SECOND;
        if second > self.current_second {
            let gap = usize::try_from(second - self.current_second).unwrap_or(usize::MAX);
            for _ in 0..gap.min(WINDOW_SECONDS) {
                self.seconds.push_back(0);
            }
            self.current_second = second;
        }
        if let Some(current) = self.seconds.back_mut() {
            *current = current.saturating_add(packets);
        }
        while self.seconds.len() > WINDOW_SECONDS {
            self.seconds.pop_front();
        }
        self.measured_ticks = self.measured_ticks.saturating_add(elapsed_ticks);
        self.last = Some(ClockMark { ticks, sequence });

        if let Some((origin_ticks, origin_instant)) = self.origin {
            let media = ticks.saturating_sub(origin_ticks) / (PCR_TICKS_PER_SECOND / 1_000);
            let wall = now.saturating_duration_since(origin_instant).as_millis();
            let media = i64::try_from(media).unwrap_or(i64::MAX);
            let wall = i64::try_from(wall).unwrap_or(i64::MAX);
            let offset = media.saturating_sub(wall);
            self.min_offset_millis = self.min_offset_millis.min(offset);
            self.max_offset_millis = self.max_offset_millis.max(offset);
        }
    }

    /// Records one packet on the video PID.
    pub(crate) fn observe_video(&mut self, packet: &[u8]) {
        let Some(decode) = pes_decode_timestamp(packet) else {
            return;
        };
        if let Some(last) = self.last_decode {
            let gap = (decode + PTS_WRAP - last) % PTS_WRAP;
            if gap > 0
                && gap < PTS_TICKS_PER_SECOND
                && let Ok(gap) = u32::try_from(gap)
            {
                self.frame_gaps.push_back(gap);
                while self.frame_gaps.len() > FRAME_SAMPLES {
                    self.frame_gaps.pop_front();
                }
            }
        }
        self.last_decode = Some(decode);
    }

    /// The measured rates, or `None` until enough media has been seen.
    pub(crate) fn rates(&self) -> Option<StreamRates> {
        // The first and last seconds are partial, so they are left out.
        let complete: Vec<u64> = self
            .seconds
            .iter()
            .copied()
            .skip(1)
            .take(self.seconds.len().saturating_sub(2))
            .collect();
        if complete.len() < MIN_COMPLETE_SECONDS {
            return None;
        }
        let bits_per_packet = (MPEG_TS_PACKET_SIZE as u64) * 8;
        let count = complete.len() as u64;
        let total: u64 = complete.iter().sum();
        let average = total.saturating_mul(bits_per_packet) / count;
        let peak = complete
            .windows(PEAK_WINDOW_SECONDS.min(complete.len()))
            .map(|window| {
                let packets: u64 = window.iter().sum();
                packets.saturating_mul(bits_per_packet) / window.len() as u64
            })
            .max()
            .unwrap_or(average)
            .max(average);
        let swing = self
            .max_offset_millis
            .saturating_sub(self.min_offset_millis);
        Some(StreamRates {
            average_bits_per_second: average,
            peak_bits_per_second: peak,
            burst: Duration::from_millis(u64::try_from(swing).unwrap_or(0)),
            frame_rate_millihertz: self.frame_rate_millihertz(),
            measured: Duration::from_millis(self.measured_ticks / (PCR_TICKS_PER_SECOND / 1_000)),
        })
    }

    fn frame_rate_millihertz(&self) -> Option<u32> {
        if self.frame_gaps.len() < MIN_FRAME_SAMPLES {
            return None;
        }
        let mut gaps: Vec<u32> = self.frame_gaps.iter().copied().collect();
        gaps.sort_unstable();
        let median = u64::from(gaps[gaps.len() / 2]);
        let measured = u32::try_from(PTS_TICKS_PER_SECOND * 1_000 / median.max(1)).ok()?;
        let snapped = STANDARD_RATES_MILLIHERTZ
            .into_iter()
            .find(|rate| measured.abs_diff(*rate) * 200 <= *rate);
        Some(snapped.unwrap_or(measured))
    }
}

/// True for the PMT stream types that carry video.
pub(crate) const fn is_video_stream_type(stream_type: u8) -> bool {
    matches!(stream_type, 0x01 | 0x02 | 0x10 | 0x1b | 0x24)
}

/// The decode timestamp of a PES that starts in this packet: the DTS when
/// present, otherwise the PTS.
fn pes_decode_timestamp(packet: &[u8]) -> Option<u64> {
    if packet.len() != MPEG_TS_PACKET_SIZE || packet[1] & 0x40 == 0 || packet[3] & 0x10 == 0 {
        return None;
    }
    let start = if packet[3] & 0x20 != 0 {
        5 + usize::from(packet[4])
    } else {
        4
    };
    let pes = packet.get(start..)?;
    if pes.len() < 14 || pes[0..3] != [0, 0, 1] {
        return None;
    }
    let flags = pes[7] >> 6;
    match flags {
        0b11 => pes.get(14..19).and_then(read_timestamp),
        0b10 => read_timestamp(&pes[9..14]),
        _ => None,
    }
}

fn read_timestamp(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 5 {
        return None;
    }
    Some(
        (u64::from(bytes[0] >> 1) & 0x07) << 30
            | u64::from(bytes[1]) << 22
            | (u64::from(bytes[2]) >> 1) << 15
            | u64::from(bytes[3]) << 7
            | u64::from(bytes[4]) >> 1,
    )
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::ring::MPEG_TS_PACKET_SIZE;

    /// A video packet that starts a PES with the given PTS and, optionally, DTS.
    pub(crate) fn pes_packet(pid: u16, pts: u64, dts: Option<u64>) -> [u8; MPEG_TS_PACKET_SIZE] {
        let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = 0x40 | u8::try_from((pid >> 8) & 0x1f).unwrap();
        packet[2] = u8::try_from(pid & 0xff).unwrap();
        packet[3] = 0x10;
        let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80];
        if let Some(dts) = dts {
            pes.extend_from_slice(&[0xc0, 10]);
            pes.extend_from_slice(&encode_timestamp(0x3, pts));
            pes.extend_from_slice(&encode_timestamp(0x1, dts));
        } else {
            pes.extend_from_slice(&[0x80, 5]);
            pes.extend_from_slice(&encode_timestamp(0x2, pts));
        }
        packet[4..4 + pes.len()].copy_from_slice(&pes);
        packet
    }

    fn encode_timestamp(prefix: u8, value: u64) -> [u8; 5] {
        let value = value % (1 << 33);
        [
            (prefix << 4) | (u8::try_from((value >> 29) & 0x0e).unwrap()) | 1,
            u8::try_from((value >> 22) & 0xff).unwrap(),
            (u8::try_from((value >> 14) & 0xfe).unwrap()) | 1,
            u8::try_from((value >> 7) & 0xff).unwrap(),
            (u8::try_from((value << 1) & 0xfe).unwrap()) | 1,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::pes_packet, *};

    const SECOND: u64 = PCR_TICKS_PER_SECOND;

    /// Feeds `seconds` of media at `packets_per_second`, with a PCR every
    /// 100 ms, delivered `speed` times faster than real time.
    fn feed(
        meter: &mut RateMeter,
        start: Instant,
        seconds: u64,
        packets_per_second: u64,
        speed: u32,
    ) {
        for tenth in 0..=seconds * 10 {
            let ticks = 1_000 * SECOND + tenth * SECOND / 10;
            let sequence = tenth * packets_per_second / 10;
            let wall = Duration::from_millis(tenth * 100) / speed;
            meter.observe_clock(ticks, sequence, start + wall);
        }
    }

    #[test]
    fn bitrate_counts_media_seconds_not_delivery_speed() {
        let start = Instant::now();
        let mut real_time = RateMeter::default();
        feed(&mut real_time, start, 20, 5_000, 1);
        let mut burst = RateMeter::default();
        feed(&mut burst, start, 20, 5_000, 4);

        let expected = 5_000 * 188 * 8;
        for meter in [&real_time, &burst] {
            let rates = meter.rates().expect("20 s is enough media");
            assert_eq!(rates.average_bits_per_second, expected);
            assert_eq!(rates.peak_bits_per_second, expected);
            assert_eq!(rates.measured, Duration::from_secs(20));
        }
        assert!(real_time.rates().unwrap().burst < Duration::from_millis(10));
        // 20 s of media in 5 s of wall time is 15 s ahead of real time.
        assert_eq!(burst.rates().unwrap().burst, Duration::from_secs(15));
    }

    #[test]
    fn peak_tracks_the_busiest_five_seconds() {
        let start = Instant::now();
        let mut meter = RateMeter::default();
        let mut sequence = 0;
        for tenth in 0..=300_u64 {
            let ticks = tenth * SECOND / 10;
            // Seconds 10 to 14 carry three times the normal rate. Packets sent
            // after a PCR count toward the second of the next PCR.
            let rate = if (99..149).contains(&tenth) {
                1_500
            } else {
                500
            };
            meter.observe_clock(ticks, sequence, start + Duration::from_millis(tenth * 100));
            sequence += rate;
        }
        let rates = meter.rates().unwrap();
        assert_eq!(rates.peak_bits_per_second, 15_000 * 188 * 8);
        assert!(rates.average_bits_per_second < rates.peak_bits_per_second);
    }

    #[test]
    fn no_rate_before_enough_media() {
        let mut meter = RateMeter::default();
        feed(&mut meter, Instant::now(), 5, 5_000, 1);
        assert_eq!(meter.rates(), None);
    }

    #[test]
    fn frame_rate_uses_decode_order_timestamps() {
        let mut meter = RateMeter::default();
        meter.set_video_pid(Some(0x100));
        // 59.94 fps with B-frames: PTS order differs from decode order, the
        // DTS advances by 1 501.5 ticks (alternating 1 501 and 1 502).
        let mut dts = 0_u64;
        for frame in 0..60_u64 {
            let pts = dts + if frame % 3 == 0 { 4_504 } else { 0 };
            meter.observe_video(&pes_packet(0x100, pts, Some(dts)));
            dts += if frame % 2 == 0 { 1_501 } else { 1_502 };
        }
        let mut meter_pts_only = RateMeter::default();
        meter_pts_only.set_video_pid(Some(0x100));
        for frame in 0..40_u64 {
            meter_pts_only.observe_video(&pes_packet(0x100, frame * 3_003, None));
        }
        feed(&mut meter, Instant::now(), 10, 1_000, 1);
        feed(&mut meter_pts_only, Instant::now(), 10, 1_000, 1);
        assert_eq!(meter.rates().unwrap().frame_rate_millihertz, Some(59_940));
        assert_eq!(
            meter_pts_only.rates().unwrap().frame_rate_millihertz,
            Some(29_970)
        );
    }

    #[test]
    fn frame_rate_survives_the_timestamp_wrap() {
        let mut meter = RateMeter::default();
        meter.set_video_pid(Some(0x100));
        let start = PTS_WRAP - 20 * 1_800;
        for frame in 0..40_u64 {
            meter.observe_video(&pes_packet(0x100, start + frame * 1_800, None));
        }
        feed(&mut meter, Instant::now(), 10, 1_000, 1);
        assert_eq!(meter.rates().unwrap().frame_rate_millihertz, Some(50_000));
    }
}
