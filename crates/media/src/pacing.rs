//! Real-time pacing of one viewer from the MPEG-TS PCR clock.
//!
//! The ring records an unwrapped media timeline for each generation
//! ([`PcrTimeline`]). A [`Pacer`] compares that timeline with a monotonic
//! clock and tells the viewer how long to hold each batch of packets. The
//! pacer takes the current time as an argument, so tests use a fake clock.

use std::{collections::VecDeque, time::Duration};

use tokio::time::Instant;

/// PCR ticks for each second (27 MHz).
pub const PCR_TICKS_PER_SECOND: u64 = 27_000_000;
/// The PCR base is 33 bits at 90 kHz. The extension multiplies it by 300.
const PCR_WRAP_TICKS: u64 = (1_u64 << 33) * 300;
/// A forward jump larger than this is a discontinuity, not playback.
const MAX_FORWARD_JUMP_TICKS: u64 = 10 * PCR_TICKS_PER_SECOND;
/// A backward step smaller than this is jitter. A larger step is a
/// discontinuity.
const MAX_BACKWARD_JITTER_TICKS: u64 = PCR_TICKS_PER_SECOND;
/// Keep one index sample for each 100 ms of media.
const SAMPLE_SPACING_TICKS: u64 = PCR_TICKS_PER_SECOND / 10;
/// The pacer never holds one batch longer than this. A longer hold means the
/// timeline is wrong, so the pacer re-anchors instead.
const MAX_HOLD: Duration = Duration::from_secs(5);

/// One index entry: the packet sequence and the media time at that packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PcrSample {
    sequence: u64,
    ticks: u64,
}

/// An unwrapped, monotonic media timeline for one ring generation.
///
/// The timeline handles the 33-bit PCR wrap and PCR discontinuities. A
/// discontinuity continues the timeline at the same media time, so it never
/// moves backward.
#[derive(Debug, Default)]
pub(crate) struct PcrTimeline {
    last_raw: Option<u64>,
    unwrapped: u64,
    latest_sequence: u64,
    has_sample: bool,
    samples: VecDeque<PcrSample>,
}

impl PcrTimeline {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Observes one transport packet on the PCR PID.
    pub(crate) fn observe(&mut self, packet: &[u8], sequence: u64) {
        let Some((raw, discontinuity)) = parse_pcr(packet) else {
            return;
        };
        match self.last_raw {
            None => {}
            Some(_) if discontinuity => {}
            Some(last) => {
                let forward = (raw + PCR_WRAP_TICKS - last) % PCR_WRAP_TICKS;
                if forward <= MAX_FORWARD_JUMP_TICKS {
                    self.unwrapped += forward;
                } else if PCR_WRAP_TICKS - forward <= MAX_BACKWARD_JITTER_TICKS {
                    // Small backward step: keep the older reference.
                    return;
                }
                // Otherwise the PCR jumped. Continue at the same media time.
            }
        }
        self.last_raw = Some(raw);
        self.latest_sequence = sequence;
        let spaced = self
            .samples
            .back()
            .is_none_or(|last| self.unwrapped >= last.ticks + SAMPLE_SPACING_TICKS);
        if spaced {
            self.samples.push_back(PcrSample {
                sequence,
                ticks: self.unwrapped,
            });
        }
        self.has_sample = true;
    }

    /// Drops index entries that the ring no longer holds. Keeps one entry
    /// before `first_sequence` so lookups near the tail still resolve.
    pub(crate) fn prune(&mut self, first_sequence: u64) {
        while self.samples.len() > 1
            && self
                .samples
                .get(1)
                .is_some_and(|next| next.sequence <= first_sequence)
        {
            self.samples.pop_front();
        }
    }

    /// Media time at the newest PCR.
    pub(crate) fn head_ticks(&self) -> Option<u64> {
        self.has_sample.then_some(self.unwrapped)
    }

    /// Media time at `sequence`: the newest indexed time at or before it.
    pub(crate) fn ticks_at(&self, sequence: u64) -> Option<u64> {
        if self.has_sample && sequence >= self.latest_sequence {
            return Some(self.unwrapped);
        }
        let index = self
            .samples
            .partition_point(|sample| sample.sequence <= sequence);
        // A position before the first PCR has the media time of that PCR.
        self.samples
            .get(index.saturating_sub(1))
            .map(|sample| sample.ticks)
    }

    /// First indexed sequence whose media time is at least `ticks`.
    pub(crate) fn sequence_at_or_after(&self, ticks: u64) -> Option<u64> {
        let index = self.samples.partition_point(|sample| sample.ticks < ticks);
        self.samples.get(index).map(|sample| sample.sequence)
    }
}

/// Returns the raw 27 MHz PCR and the discontinuity flag, if the packet has a PCR.
fn parse_pcr(packet: &[u8]) -> Option<(u64, bool)> {
    if packet.len() < 12 || packet[3] & 0x20 == 0 {
        return None;
    }
    let adaptation_length = usize::from(packet[4]);
    if adaptation_length < 7 || packet[5] & 0x10 == 0 {
        return None;
    }
    let discontinuity = packet[5] & 0x80 != 0;
    let base = (u64::from(packet[6]) << 25)
        | (u64::from(packet[7]) << 17)
        | (u64::from(packet[8]) << 9)
        | (u64::from(packet[9]) << 1)
        | (u64::from(packet[10]) >> 7);
    let extension = (u64::from(packet[10] & 0x01) << 8) | u64::from(packet[11]);
    Some((base * 300 + extension.min(299), discontinuity))
}

/// What the pacer knows about the position of the next batch.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BatchPosition {
    /// Sequence of the first packet in the batch.
    pub sequence: u64,
    /// Media time from the PCR timeline, if the timeline covers the batch.
    pub ticks: Option<u64>,
    /// Measured stream rate in bits per second, for the byte-rate fallback.
    pub byte_rate_bps: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
enum Anchor {
    Pcr { instant: Instant, ticks: u64 },
    Bytes { instant: Instant, sequence: u64 },
}

/// Holds each batch until its media time is due on the monotonic clock.
#[derive(Debug, Default)]
pub(crate) struct Pacer {
    anchor: Option<Anchor>,
}

impl Pacer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Forget the clock reference. Call this after a lag skip, a generation
    /// boundary, or an underrun. The next batch starts a new reference.
    pub(crate) fn reset(&mut self) {
        self.anchor = None;
    }

    /// Returns how long to hold the batch at `position`.
    pub(crate) fn hold(&mut self, position: BatchPosition, now: Instant) -> Duration {
        if let Some(ticks) = position.ticks {
            self.hold_pcr(ticks, now, position)
        } else if let Some(rate) = position.byte_rate_bps.filter(|rate| *rate > 0) {
            self.hold_bytes(position.sequence, rate, now, position)
        } else {
            // No clock source yet. Release at once, as an unpaced viewer.
            self.anchor = None;
            Duration::ZERO
        }
    }

    fn hold_pcr(&mut self, ticks: u64, now: Instant, position: BatchPosition) -> Duration {
        let Some(Anchor::Pcr {
            instant,
            ticks: anchor_ticks,
        }) = self.anchor
        else {
            self.anchor = Some(Anchor::Pcr {
                instant: now,
                ticks,
            });
            return Duration::ZERO;
        };
        let elapsed = ticks.saturating_sub(anchor_ticks);
        let due = instant + ticks_to_duration(elapsed);
        let hold = due.saturating_duration_since(now);
        if hold > MAX_HOLD {
            self.anchor = None;
            return self.hold(position, now);
        }
        hold
    }

    fn hold_bytes(
        &mut self,
        sequence: u64,
        rate_bps: u64,
        now: Instant,
        position: BatchPosition,
    ) -> Duration {
        let Some(Anchor::Bytes {
            instant,
            sequence: anchor_sequence,
        }) = self.anchor
        else {
            self.anchor = Some(Anchor::Bytes {
                instant: now,
                sequence,
            });
            return Duration::ZERO;
        };
        let packets = sequence.saturating_sub(anchor_sequence);
        let bits = u128::from(packets) * 188 * 8;
        let nanos = bits * 1_000_000_000 / u128::from(rate_bps);
        let due = instant + Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
        let hold = due.saturating_duration_since(now);
        if hold > MAX_HOLD {
            self.anchor = None;
            return self.hold(position, now);
        }
        hold
    }
}

fn ticks_to_duration(ticks: u64) -> Duration {
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(PCR_TICKS_PER_SECOND);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Converts PCR ticks to milliseconds.
pub(crate) fn ticks_to_millis(ticks: u64) -> u64 {
    ticks / (PCR_TICKS_PER_SECOND / 1_000)
}

/// Converts a duration to PCR ticks.
pub(crate) fn duration_to_ticks(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos() * u128::from(PCR_TICKS_PER_SECOND) / 1_000_000_000)
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::MPEG_TS_PACKET_SIZE;

    /// Builds a packet on `pid` with a PCR value in 27 MHz ticks.
    pub(crate) fn pcr_packet(pid: u16, pcr_ticks: u64, discontinuity: bool) -> [u8; 188] {
        let base = pcr_ticks / 300;
        let extension = pcr_ticks % 300;
        let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = u8::try_from((pid >> 8) & 0x1f).unwrap();
        packet[2] = u8::try_from(pid & 0xff).unwrap();
        packet[3] = 0x30;
        packet[4] = 183;
        packet[5] = 0x10 | if discontinuity { 0x80 } else { 0 };
        packet[6] = u8::try_from((base >> 25) & 0xff).unwrap();
        packet[7] = u8::try_from((base >> 17) & 0xff).unwrap();
        packet[8] = u8::try_from((base >> 9) & 0xff).unwrap();
        packet[9] = u8::try_from((base >> 1) & 0xff).unwrap();
        packet[10] = u8::try_from(((base & 1) << 7) | 0x7e | (extension >> 8)).unwrap();
        packet[11] = u8::try_from(extension & 0xff).unwrap();
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::pcr_packet;
    use super::*;

    const SECOND: u64 = PCR_TICKS_PER_SECOND;

    #[test]
    fn parses_pcr_values() {
        let packet = pcr_packet(0x101, 5 * SECOND + 123, false);
        assert_eq!(parse_pcr(&packet), Some((5 * SECOND + 123, false)));
        let packet = pcr_packet(0x101, 7, true);
        assert_eq!(parse_pcr(&packet), Some((7, true)));
    }

    #[test]
    fn timeline_unwraps_the_33_bit_base() {
        let mut timeline = PcrTimeline::default();
        let start = PCR_WRAP_TICKS - 2 * SECOND;
        for step in 0..8_u64 {
            let raw = (start + step * SECOND / 2) % PCR_WRAP_TICKS;
            timeline.observe(&pcr_packet(0x101, raw, false), step);
        }
        // Eight samples at 0.5 s spacing cover 3.5 s across the wrap.
        assert_eq!(timeline.head_ticks(), Some(7 * SECOND / 2));
        assert_eq!(timeline.ticks_at(0), Some(0));
        assert_eq!(timeline.ticks_at(5), Some(5 * SECOND / 2));
    }

    #[test]
    fn timeline_continues_across_a_discontinuity() {
        let mut timeline = PcrTimeline::default();
        timeline.observe(&pcr_packet(0x101, 100 * SECOND, false), 0);
        timeline.observe(&pcr_packet(0x101, 101 * SECOND, false), 1);
        // The flag marks a jump back to near zero.
        timeline.observe(&pcr_packet(0x101, 3 * SECOND, true), 2);
        assert_eq!(timeline.head_ticks(), Some(SECOND));
        timeline.observe(&pcr_packet(0x101, 4 * SECOND, false), 3);
        assert_eq!(timeline.head_ticks(), Some(2 * SECOND));
        // An unflagged large jump is also a discontinuity.
        timeline.observe(&pcr_packet(0x101, 500 * SECOND, false), 4);
        assert_eq!(timeline.head_ticks(), Some(2 * SECOND));
        timeline.observe(&pcr_packet(0x101, 501 * SECOND, false), 5);
        assert_eq!(timeline.head_ticks(), Some(3 * SECOND));
    }

    #[test]
    fn timeline_ignores_small_backward_jitter() {
        let mut timeline = PcrTimeline::default();
        timeline.observe(&pcr_packet(0x101, 10 * SECOND, false), 0);
        timeline.observe(&pcr_packet(0x101, 11 * SECOND, false), 1);
        timeline.observe(&pcr_packet(0x101, 11 * SECOND - 1_000, false), 2);
        assert_eq!(timeline.head_ticks(), Some(SECOND));
    }

    #[test]
    fn timeline_locates_a_sequence_for_a_media_time() {
        let mut timeline = PcrTimeline::default();
        for second in 0..10_u64 {
            timeline.observe(&pcr_packet(0x101, second * SECOND, false), second * 100);
        }
        assert_eq!(timeline.sequence_at_or_after(4 * SECOND), Some(400));
        assert_eq!(timeline.ticks_at(450), Some(4 * SECOND));
        timeline.prune(500);
        assert_eq!(timeline.sequence_at_or_after(0), Some(500));
    }

    fn position(sequence: u64, millis: u64) -> BatchPosition {
        BatchPosition {
            sequence,
            ticks: Some(millis * (SECOND / 1_000)),
            byte_rate_bps: None,
        }
    }

    #[test]
    fn pacer_holds_batches_to_the_media_clock() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        assert_eq!(pacer.hold(position(0, 100_000), start), Duration::ZERO);
        // A burst: media 1 s ahead, no wall time has passed.
        assert_eq!(
            pacer.hold(position(10, 101_000), start),
            Duration::from_secs(1)
        );
        // Half a second later, the same batch needs half a second more.
        assert_eq!(
            pacer.hold(position(10, 101_000), start + Duration::from_millis(500)),
            Duration::from_millis(500)
        );
        // A late batch is released at once.
        assert_eq!(
            pacer.hold(position(20, 101_500), start + Duration::from_secs(3)),
            Duration::ZERO
        );
    }

    #[test]
    fn pacer_reset_starts_a_new_reference() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        pacer.hold(position(0, 100_000), start);
        pacer.reset();
        // The new generation restarts its PCR near zero.
        assert_eq!(pacer.hold(position(0, 2000), start), Duration::ZERO);
        assert_eq!(pacer.hold(position(5, 3000), start), Duration::from_secs(1));
    }

    #[test]
    fn pacer_re_anchors_after_an_implausible_hold() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        pacer.hold(position(0, 0), start);
        assert_eq!(pacer.hold(position(1, 3_600_000), start), Duration::ZERO);
    }

    #[test]
    fn pacer_falls_back_to_the_byte_rate() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        // 188 bytes * 8 = 1_504 bits per packet. At 1_504 kbit/s, 1000
        // packets take one second.
        let rate = Some(1_504_000);
        let at = |sequence| BatchPosition {
            sequence,
            ticks: None,
            byte_rate_bps: rate,
        };
        assert_eq!(pacer.hold(at(0), start), Duration::ZERO);
        assert_eq!(pacer.hold(at(1_000), start), Duration::from_secs(1));
        assert_eq!(
            pacer.hold(at(1_000), start + Duration::from_millis(400)),
            Duration::from_millis(600)
        );
    }

    #[test]
    fn pacer_without_any_clock_source_releases_at_once() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        let none = BatchPosition {
            sequence: 5,
            ticks: None,
            byte_rate_bps: None,
        };
        assert_eq!(pacer.hold(none, start), Duration::ZERO);
    }
}
