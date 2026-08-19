//! A non-blocking, packet-aligned MPEG transport stream ring.

use std::{collections::VecDeque, fmt, sync::Arc, time::Duration};

use bytes::{Bytes, BytesMut};
use parking_lot::Mutex;
use thiserror::Error;
use tokio::sync::Notify;

use crate::psi::PatPmtTracker;

pub const MPEG_TS_PACKET_SIZE: usize = 188;
pub const DEFAULT_PRE_ROLL: Duration = Duration::from_secs(2);

/// Capacity and initial-reader position for an [`MpegTsRing`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MpegTsRingConfig {
    capacity_packets: usize,
    pre_roll_packets: usize,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RingConfigError {
    #[error("an MPEG-TS ring must hold at least one packet")]
    EmptyCapacity,
    #[error("pre-roll ({pre_roll_packets} packets) exceeds ring capacity ({capacity_packets})")]
    PreRollExceedsCapacity {
        capacity_packets: usize,
        pre_roll_packets: usize,
    },
    #[error("estimated bitrate must be greater than zero")]
    ZeroBitrate,
    #[error("duration and bitrate produce a packet count too large for this platform")]
    CapacityOverflow,
}

impl MpegTsRingConfig {
    /// # Errors
    ///
    /// Returns an error for zero/overflowing capacity or pre-roll larger than
    /// the capacity.
    pub fn new(capacity_packets: usize, pre_roll_packets: usize) -> Result<Self, RingConfigError> {
        if capacity_packets == 0 {
            return Err(RingConfigError::EmptyCapacity);
        }
        capacity_packets
            .checked_mul(MPEG_TS_PACKET_SIZE)
            .ok_or(RingConfigError::CapacityOverflow)?;
        if pre_roll_packets > capacity_packets {
            return Err(RingConfigError::PreRollExceedsCapacity {
                capacity_packets,
                pre_roll_packets,
            });
        }
        Ok(Self {
            capacity_packets,
            pre_roll_packets,
        })
    }

    /// Sizes the ring from a nominal stream bitrate and time windows.
    ///
    /// Both durations are rounded up to whole 188-byte packets. A typical live
    /// configuration uses a buffer larger than `pre_roll`, with
    /// [`DEFAULT_PRE_ROLL`] as the latter.
    ///
    /// # Errors
    ///
    /// Returns an error for zero bitrate, zero/overflowing capacity, or a
    /// pre-roll duration larger than the buffer duration after rounding.
    pub fn for_bitrate(
        bitrate_bits_per_second: u64,
        buffer_duration: Duration,
        pre_roll: Duration,
    ) -> Result<Self, RingConfigError> {
        if bitrate_bits_per_second == 0 {
            return Err(RingConfigError::ZeroBitrate);
        }
        let capacity_packets = packets_for_duration(bitrate_bits_per_second, buffer_duration)?;
        let pre_roll_packets = packets_for_duration(bitrate_bits_per_second, pre_roll)?;
        Self::new(capacity_packets, pre_roll_packets)
    }

    /// Uses [`DEFAULT_PRE_ROLL`] with the supplied buffer duration.
    ///
    /// # Errors
    ///
    /// Returns the same configuration errors as [`Self::for_bitrate`].
    pub fn with_default_pre_roll(
        bitrate_bits_per_second: u64,
        buffer_duration: Duration,
    ) -> Result<Self, RingConfigError> {
        Self::for_bitrate(bitrate_bits_per_second, buffer_duration, DEFAULT_PRE_ROLL)
    }

    pub fn capacity_packets(self) -> usize {
        self.capacity_packets
    }

    pub fn pre_roll_packets(self) -> usize {
        self.pre_roll_packets
    }

    pub fn capacity_bytes(self) -> usize {
        self.capacity_packets * MPEG_TS_PACKET_SIZE
    }
}

fn packets_for_duration(
    bitrate_bits_per_second: u64,
    duration: Duration,
) -> Result<usize, RingConfigError> {
    let bit_nanoseconds = u128::from(bitrate_bits_per_second)
        .checked_mul(duration.as_nanos())
        .ok_or(RingConfigError::CapacityOverflow)?;
    let bits_per_packet_nanoseconds = (MPEG_TS_PACKET_SIZE as u128) * 8 * 1_000_000_000;
    let packets = bit_nanoseconds
        .checked_add(bits_per_packet_nanoseconds - 1)
        .ok_or(RingConfigError::CapacityOverflow)?
        / bits_per_packet_nanoseconds;
    usize::try_from(packets).map_err(|_| RingConfigError::CapacityOverflow)
}

/// Why a generation stopped producing bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RingCloseReason {
    EndOfStream,
    Shutdown,
    UpstreamError(Arc<str>),
    RecoveryExpired { attempts: u64 },
}

/// A bounded ring. Writers never wait for readers; lag is reported per cursor.
#[derive(Clone)]
pub struct MpegTsRing {
    inner: Arc<RingInner>,
}

impl fmt::Debug for MpegTsRing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MpegTsRing")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

#[derive(Debug)]
struct RingInner {
    config: MpegTsRingConfig,
    state: Mutex<RingState>,
    changed: Notify,
}

#[derive(Debug)]
struct RingState {
    storage: Box<[u8]>,
    generation: u64,
    first_sequence: u64,
    next_sequence: u64,
    safe_boundaries: VecDeque<u64>,
    psi: PatPmtTracker,
    closed: Option<RingCloseReason>,
}

/// Immutable ring accounting useful for health and diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RingSnapshot {
    pub generation: u64,
    pub first_sequence: u64,
    pub next_sequence: u64,
    pub retained_packets: usize,
    pub capacity_packets: usize,
    pub closed: Option<RingCloseReason>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RingWriteError {
    #[error("write length {bytes} is not a multiple of the 188-byte MPEG-TS packet size")]
    Unaligned { bytes: usize },
    #[error("packet {packet_index} has sync byte {actual:#04x}, expected 0x47")]
    InvalidSyncByte { packet_index: usize, actual: u8 },
    #[error("ring generation {generation} is closed: {reason:?}")]
    Closed {
        generation: u64,
        reason: RingCloseReason,
    },
    #[error("ring generation exhausted its packet sequence space")]
    SequenceExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteOutcome {
    pub generation: u64,
    pub packets_written: usize,
    pub packets_overwritten: u64,
    pub first_sequence: u64,
    pub next_sequence: u64,
}

/// One reader's independent position in the ring.
#[derive(Debug)]
pub struct RingCursor {
    ring: MpegTsRing,
    generation: u64,
    next_sequence: u64,
    waiting_for_safe_boundary: bool,
}

/// A cursor event. Lag and generation transitions are explicit boundaries;
/// packet data is never silently joined across either condition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RingRead {
    Packets {
        generation: u64,
        first_sequence: u64,
        bytes: Bytes,
    },
    Lagged {
        generation: u64,
        skipped_packets: u64,
        resume_at: u64,
    },
    GenerationBoundary {
        previous_generation: u64,
        generation: u64,
    },
    Closed {
        generation: u64,
        reason: RingCloseReason,
    },
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RingReadError {
    #[error("a cursor read must request at least one packet")]
    EmptyRead,
}

impl MpegTsRing {
    pub fn new(config: MpegTsRingConfig) -> Self {
        Self {
            inner: Arc::new(RingInner {
                config,
                state: Mutex::new(RingState {
                    storage: vec![0; config.capacity_bytes()].into_boxed_slice(),
                    generation: 0,
                    first_sequence: 0,
                    next_sequence: 0,
                    safe_boundaries: VecDeque::new(),
                    psi: PatPmtTracker::default(),
                    closed: None,
                }),
                changed: Notify::new(),
            }),
        }
    }

    /// Writes complete MPEG-TS packets, overwriting old data when necessary.
    ///
    /// A slow reader can never block this operation or close an upstream.
    ///
    /// # Errors
    ///
    /// Returns an error for unaligned data, invalid sync bytes, a closed ring,
    /// or exhausted sequence space.
    ///
    /// # Panics
    ///
    /// Panics only on a platform whose `usize` values cannot be represented by
    /// `u64`; such a capacity is rejected by the configuration constructor.
    pub fn push(&self, bytes: &[u8]) -> Result<WriteOutcome, RingWriteError> {
        if !bytes.len().is_multiple_of(MPEG_TS_PACKET_SIZE) {
            return Err(RingWriteError::Unaligned { bytes: bytes.len() });
        }
        for (packet_index, packet) in bytes.chunks_exact(MPEG_TS_PACKET_SIZE).enumerate() {
            if packet[0] != 0x47 {
                return Err(RingWriteError::InvalidSyncByte {
                    packet_index,
                    actual: packet[0],
                });
            }
        }

        let packet_count = bytes.len() / MPEG_TS_PACKET_SIZE;
        let mut state = self.inner.state.lock();
        if let Some(reason) = &state.closed {
            return Err(RingWriteError::Closed {
                generation: state.generation,
                reason: reason.clone(),
            });
        }
        let increment =
            u64::try_from(packet_count).map_err(|_| RingWriteError::SequenceExhausted)?;
        state
            .next_sequence
            .checked_add(increment)
            .ok_or(RingWriteError::SequenceExhausted)?;

        let old_first = state.first_sequence;
        for packet in bytes.chunks_exact(MPEG_TS_PACKET_SIZE) {
            let sequence = state.next_sequence;
            if let Some(boundary) = state.psi.observe(packet, sequence) {
                state.safe_boundaries.push_back(boundary);
            }
            let slot = usize::try_from(
                sequence % u64::try_from(self.inner.config.capacity_packets).unwrap(),
            )
            .unwrap();
            let offset = slot * MPEG_TS_PACKET_SIZE;
            state.storage[offset..offset + MPEG_TS_PACKET_SIZE].copy_from_slice(packet);
            state.next_sequence += 1;
            state.first_sequence = state
                .next_sequence
                .saturating_sub(u64::try_from(self.inner.config.capacity_packets).unwrap());
        }
        while state
            .safe_boundaries
            .front()
            .is_some_and(|boundary| *boundary < state.first_sequence)
        {
            state.safe_boundaries.pop_front();
        }
        if state
            .psi
            .candidate_start()
            .is_some_and(|boundary| boundary < state.first_sequence)
        {
            state.psi.reset();
        }

        let outcome = WriteOutcome {
            generation: state.generation,
            packets_written: packet_count,
            packets_overwritten: state.first_sequence.saturating_sub(old_first),
            first_sequence: state.first_sequence,
            next_sequence: state.next_sequence,
        };
        drop(state);
        if packet_count > 0 {
            self.inner.changed.notify_waiters();
        }
        Ok(outcome)
    }

    /// Starts a clean generation and wakes every cursor.
    ///
    /// Old and new packets can therefore never be returned in one read.
    ///
    /// # Errors
    ///
    /// Returns [`RingWriteError::SequenceExhausted`] if the generation counter
    /// is exhausted.
    pub fn start_new_generation(&self) -> Result<u64, RingWriteError> {
        let mut state = self.inner.state.lock();
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(RingWriteError::SequenceExhausted)?;
        state.first_sequence = 0;
        state.next_sequence = 0;
        state.safe_boundaries.clear();
        state.psi.reset();
        state.closed = None;
        let generation = state.generation;
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(generation)
    }

    /// Marks the current generation closed and wakes waiting cursors.
    pub fn close(&self, reason: RingCloseReason) -> bool {
        let mut state = self.inner.state.lock();
        if state.closed.is_some() {
            return false;
        }
        state.closed = Some(reason);
        drop(state);
        self.inner.changed.notify_waiters();
        true
    }

    /// Creates a cursor beginning at the configured pre-roll behind live edge.
    pub fn subscribe(&self) -> RingCursor {
        self.subscribe_with_pre_roll(self.inner.config.pre_roll_packets)
    }

    /// Creates a cursor at the live edge, with no retained packets replayed.
    pub fn subscribe_at_live_edge(&self) -> RingCursor {
        self.subscribe_with_pre_roll(0)
    }

    /// Creates a cursor with a caller-selected pre-roll, capped to retained data.
    pub fn subscribe_with_pre_roll(&self, pre_roll_packets: usize) -> RingCursor {
        let state = self.inner.state.lock();
        let requested = u64::try_from(pre_roll_packets).unwrap_or(u64::MAX);
        let target = state
            .next_sequence
            .saturating_sub(requested)
            .max(state.first_sequence);
        let (next_sequence, waiting_for_safe_boundary) = safe_resume_position(&state, target);
        RingCursor {
            ring: self.clone(),
            generation: state.generation,
            next_sequence,
            waiting_for_safe_boundary,
        }
    }

    /// # Panics
    ///
    /// Panics only if an internal retained-packet count, which is bounded by a
    /// validated `usize` capacity, cannot convert back to `usize`.
    pub fn snapshot(&self) -> RingSnapshot {
        let state = self.inner.state.lock();
        RingSnapshot {
            generation: state.generation,
            first_sequence: state.first_sequence,
            next_sequence: state.next_sequence,
            retained_packets: usize::try_from(state.next_sequence - state.first_sequence).unwrap(),
            capacity_packets: self.inner.config.capacity_packets,
            closed: state.closed.clone(),
        }
    }
}

impl RingCursor {
    /// Waits until data, lag, a generation boundary, or close is observable.
    ///
    /// # Errors
    ///
    /// Returns [`RingReadError::EmptyRead`] if `max_packets` is zero.
    pub async fn next(&mut self, max_packets: usize) -> Result<RingRead, RingReadError> {
        if max_packets == 0 {
            return Err(RingReadError::EmptyRead);
        }

        let inner = Arc::clone(&self.ring.inner);
        loop {
            // Register before checking state so a notification cannot be lost
            // between the check and await.
            let changed = inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(event) = self.try_next(max_packets)? {
                return Ok(event);
            }
            changed.await;
        }
    }

    /// Non-blocking form of [`Self::next`]. `None` means the cursor is at a
    /// live, open edge.
    ///
    /// # Errors
    ///
    /// Returns [`RingReadError::EmptyRead`] if `max_packets` is zero.
    ///
    /// # Panics
    ///
    /// Panics only if internally bounded sequence/count conversions cannot fit
    /// the platform's `usize`.
    pub fn try_next(&mut self, max_packets: usize) -> Result<Option<RingRead>, RingReadError> {
        if max_packets == 0 {
            return Err(RingReadError::EmptyRead);
        }

        let state = self.ring.inner.state.lock();
        if self.generation != state.generation {
            let previous_generation = self.generation;
            self.generation = state.generation;
            // An existing viewer crossing a supervised recovery generation
            // resumes at that generation's first confirmed PAT/PMT boundary,
            // irrespective of the new-viewer pre-roll preference.
            let target = state.first_sequence;
            (self.next_sequence, self.waiting_for_safe_boundary) =
                safe_resume_position(&state, target);
            return Ok(Some(RingRead::GenerationBoundary {
                previous_generation,
                generation: state.generation,
            }));
        }

        if self.next_sequence < state.first_sequence {
            let previous_sequence = self.next_sequence;
            (self.next_sequence, self.waiting_for_safe_boundary) =
                safe_resume_position(&state, state.first_sequence);
            return Ok(Some(RingRead::Lagged {
                generation: state.generation,
                skipped_packets: self.next_sequence.saturating_sub(previous_sequence),
                resume_at: self.next_sequence,
            }));
        }

        if self.waiting_for_safe_boundary {
            if let Some(boundary) = state
                .safe_boundaries
                .iter()
                .copied()
                .find(|boundary| *boundary >= self.next_sequence)
            {
                self.next_sequence = boundary;
                self.waiting_for_safe_boundary = false;
            } else {
                if let Some(candidate) = state
                    .psi
                    .candidate_start()
                    .filter(|candidate| *candidate >= self.next_sequence)
                {
                    self.next_sequence = candidate;
                } else {
                    self.next_sequence = state.next_sequence;
                }
                return Ok(state.closed.as_ref().map(|reason| RingRead::Closed {
                    generation: state.generation,
                    reason: reason.clone(),
                }));
            }
        }

        if self.next_sequence < state.next_sequence {
            let available = state.next_sequence - self.next_sequence;
            let count = available.min(u64::try_from(max_packets).unwrap_or(u64::MAX));
            let count = usize::try_from(count).unwrap();
            let first_sequence = self.next_sequence;
            let mut bytes = BytesMut::with_capacity(count * MPEG_TS_PACKET_SIZE);
            for sequence in first_sequence..first_sequence + u64::try_from(count).unwrap() {
                let slot = usize::try_from(
                    sequence % u64::try_from(self.ring.inner.config.capacity_packets).unwrap(),
                )
                .unwrap();
                let offset = slot * MPEG_TS_PACKET_SIZE;
                bytes.extend_from_slice(&state.storage[offset..offset + MPEG_TS_PACKET_SIZE]);
            }
            self.next_sequence += u64::try_from(count).unwrap();
            return Ok(Some(RingRead::Packets {
                generation: state.generation,
                first_sequence,
                bytes: bytes.freeze(),
            }));
        }

        Ok(state.closed.as_ref().map(|reason| RingRead::Closed {
            generation: state.generation,
            reason: reason.clone(),
        }))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
}

fn safe_resume_position(state: &RingState, target: u64) -> (u64, bool) {
    if let Some(boundary) = state
        .safe_boundaries
        .iter()
        .copied()
        .find(|boundary| *boundary >= target)
    {
        return (boundary, false);
    }
    if let Some(candidate) = state
        .psi
        .candidate_start()
        .filter(|candidate| *candidate >= target)
    {
        return (candidate, true);
    }
    (state.next_sequence, true)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use tokio::time::{Duration, timeout};

    use crate::psi::test_support::{TEST_VIDEO_PID, pat_packet, pmt_packet};

    use super::*;

    fn packet(id: u32) -> [u8; MPEG_TS_PACKET_SIZE] {
        let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = u8::try_from((TEST_VIDEO_PID >> 8) & 0x1f).unwrap();
        packet[2] = u8::try_from(TEST_VIDEO_PID & 0xff).unwrap();
        packet[3] = 0x10;
        packet[4..8].copy_from_slice(&id.to_be_bytes());
        packet
    }

    fn packets(ids: impl IntoIterator<Item = u32>) -> Vec<u8> {
        ids.into_iter().flat_map(packet).collect()
    }

    fn packet_ids(bytes: &Bytes) -> Vec<u32> {
        bytes
            .chunks_exact(MPEG_TS_PACKET_SIZE)
            .filter(|packet| {
                ((u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])) == TEST_VIDEO_PID
            })
            .map(|packet| u32::from_be_bytes(packet[4..8].try_into().unwrap()))
            .collect()
    }

    fn safe_packets(ids: impl IntoIterator<Item = u32>) -> Vec<u8> {
        [pat_packet().to_vec(), pmt_packet().to_vec(), packets(ids)].concat()
    }

    #[test]
    fn bitrate_configuration_produces_two_second_style_pre_roll() {
        let config =
            MpegTsRingConfig::with_default_pre_roll(8_000_000, Duration::from_secs(10)).unwrap();

        // 8 Mbps is 1,000,000 bytes/s; both values round up to packets.
        assert_eq!(config.capacity_packets(), 53_192);
        assert_eq!(config.pre_roll_packets(), 10_639);
        assert_eq!(
            MpegTsRingConfig::new(2, 3),
            Err(RingConfigError::PreRollExceedsCapacity {
                capacity_packets: 2,
                pre_roll_packets: 3
            })
        );
    }

    #[tokio::test]
    async fn configuration_debug_empty_writes_and_cursor_accessors_are_explicit() {
        assert_eq!(
            MpegTsRingConfig::new(0, 0),
            Err(RingConfigError::EmptyCapacity)
        );
        assert_eq!(
            MpegTsRingConfig::for_bitrate(0, Duration::from_secs(1), Duration::ZERO),
            Err(RingConfigError::ZeroBitrate)
        );

        let config = MpegTsRingConfig::new(2, 1).unwrap();
        assert_eq!(config.capacity_packets(), 2);
        assert_eq!(config.pre_roll_packets(), 1);
        assert_eq!(config.capacity_bytes(), 2 * MPEG_TS_PACKET_SIZE);
        let ring = MpegTsRing::new(config);
        assert!(format!("{ring:?}").contains("snapshot"));

        let outcome = ring.push(&[]).unwrap();
        assert_eq!(outcome.packets_written, 0);
        assert_eq!(outcome.first_sequence, 0);
        assert_eq!(outcome.next_sequence, 0);

        let mut cursor = ring.subscribe_at_live_edge();
        assert_eq!(cursor.generation(), 0);
        assert_eq!(cursor.next_sequence(), 0);
        assert_eq!(cursor.try_next(0), Err(RingReadError::EmptyRead));
        assert_eq!(cursor.next(0).await, Err(RingReadError::EmptyRead));
    }

    #[test]
    fn rejects_unaligned_or_unsynchronized_writes_without_mutation() {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(4, 0).unwrap());
        assert_eq!(
            ring.push(&[0; 187]),
            Err(RingWriteError::Unaligned { bytes: 187 })
        );
        assert_eq!(
            ring.push(&[0; MPEG_TS_PACKET_SIZE]),
            Err(RingWriteError::InvalidSyncByte {
                packet_index: 0,
                actual: 0
            })
        );
        assert_eq!(ring.snapshot().retained_packets, 0);
    }

    #[tokio::test]
    async fn pre_roll_and_independent_cursors_preserve_packet_order() {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(12, 4).unwrap());
        ring.push(&safe_packets([0, 1])).unwrap();
        ring.push(&packet(2)).unwrap();
        ring.push(&safe_packets([3, 4])).unwrap();
        let mut pre_roll = ring.subscribe();
        let mut oldest = ring.subscribe_with_pre_roll(99);

        let RingRead::Packets {
            first_sequence,
            bytes,
            ..
        } = pre_roll.next(8).await.unwrap()
        else {
            panic!("expected packets");
        };
        assert_eq!(first_sequence, 5);
        assert_eq!(packet_ids(&bytes), [3, 4]);

        let RingRead::Packets { bytes, .. } = oldest.next(4).await.unwrap() else {
            panic!("expected packets");
        };
        assert_eq!(packet_ids(&bytes), [0, 1]);
        let RingRead::Packets { bytes, .. } = oldest.next(8).await.unwrap() else {
            panic!("expected packets");
        };
        assert_eq!(packet_ids(&bytes), [2, 3, 4]);
    }

    #[tokio::test]
    async fn wrap_reports_lag_and_never_closes_or_backpressures_upstream() {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(4, 4).unwrap());
        ring.push(&safe_packets([0])).unwrap();
        let mut slow = ring.subscribe_with_pre_roll(4);

        for id in 1..20 {
            let outcome = ring.push(&packet(id)).unwrap();
            assert_eq!(outcome.packets_written, 1);
            assert!(ring.snapshot().closed.is_none());
        }
        ring.push(&safe_packets([100])).unwrap();
        let safe_start = ring.snapshot().next_sequence - 3;

        assert_eq!(
            slow.next(10).await.unwrap(),
            RingRead::Lagged {
                generation: 0,
                skipped_packets: safe_start,
                resume_at: safe_start,
            }
        );
        let RingRead::Packets { bytes, .. } = slow.next(10).await.unwrap() else {
            panic!("expected retained packets after lag");
        };
        assert_eq!(&bytes[..MPEG_TS_PACKET_SIZE], &pat_packet());
        assert_eq!(packet_ids(&bytes), [100]);

        // Reader lag has no control path that can close or stall the writer.
        ring.push(&packet(20)).unwrap();
        assert!(ring.snapshot().closed.is_none());
    }

    #[tokio::test]
    async fn generation_boundary_prevents_old_and_new_packets_from_mixing() {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(5, 5).unwrap());
        ring.push(&safe_packets([1, 2])).unwrap();
        let mut cursor = ring.subscribe_with_pre_roll(5);

        assert_eq!(ring.start_new_generation().unwrap(), 1);
        ring.push(&safe_packets([10, 11])).unwrap();

        assert_eq!(
            cursor.next(4).await.unwrap(),
            RingRead::GenerationBoundary {
                previous_generation: 0,
                generation: 1,
            }
        );
        let RingRead::Packets {
            generation, bytes, ..
        } = cursor.next(4).await.unwrap()
        else {
            panic!("expected new generation packets");
        };
        assert_eq!(generation, 1);
        assert_eq!(packet_ids(&bytes), [10, 11]);
    }

    #[tokio::test]
    async fn close_wakes_waiters_after_buffered_packets_are_drained() {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(4, 0).unwrap());
        let mut waiting = ring.subscribe_at_live_edge();
        let reader = tokio::spawn(async move { waiting.next(4).await.unwrap() });
        tokio::task::yield_now().await;

        assert!(ring.close(RingCloseReason::EndOfStream));
        assert!(!ring.close(RingCloseReason::Shutdown));
        assert_eq!(
            timeout(Duration::from_secs(1), reader)
                .await
                .unwrap()
                .unwrap(),
            RingRead::Closed {
                generation: 0,
                reason: RingCloseReason::EndOfStream
            }
        );
        assert!(matches!(
            ring.push(&packet(1)),
            Err(RingWriteError::Closed { .. })
        ));

        let draining = MpegTsRing::new(MpegTsRingConfig::new(4, 4).unwrap());
        draining.push(&safe_packets([1, 2])).unwrap();
        let mut cursor = draining.subscribe();
        draining.close(RingCloseReason::Shutdown);
        assert!(matches!(
            cursor.next(4).await.unwrap(),
            RingRead::Packets { .. }
        ));
        assert!(matches!(
            cursor.next(4).await.unwrap(),
            RingRead::Closed { .. }
        ));
    }

    #[test]
    fn a_new_viewer_waits_for_a_confirmed_pat_pmt_boundary() {
        let ring = MpegTsRing::new(MpegTsRingConfig::new(8, 8).unwrap());
        ring.push(&packets([1, 2])).unwrap();
        let mut cursor = ring.subscribe_with_pre_roll(8);
        assert_eq!(cursor.try_next(8).unwrap(), None);

        ring.push(&pat_packet()).unwrap();
        assert_eq!(cursor.try_next(8).unwrap(), None);
        ring.push(&[pmt_packet().to_vec(), packet(3).to_vec()].concat())
            .unwrap();
        let Some(RingRead::Packets { bytes, .. }) = cursor.try_next(8).unwrap() else {
            panic!("expected a PAT/PMT-safe restart");
        };
        assert_eq!(&bytes[..MPEG_TS_PACKET_SIZE], &pat_packet());
        assert_eq!(packet_ids(&bytes), [3]);
    }

    proptest! {
        #[test]
        fn ring_retains_exactly_the_newest_complete_packets(
            capacity in 3_usize..64,
            write_count in 0_usize..512,
        ) {
            let ring = MpegTsRing::new(MpegTsRingConfig::new(capacity, capacity).unwrap());
            let ids = (0..u32::try_from(write_count).unwrap()).collect::<Vec<_>>();
            ring.push(&packets(ids.clone())).unwrap();
            ring.push(&safe_packets([u32::MAX])).unwrap();
            let mut cursor = ring.subscribe_with_pre_roll(usize::MAX);

            let event = cursor.try_next(capacity.max(1)).unwrap();
            let Some(RingRead::Packets { bytes, .. }) = event else {
                return Err(TestCaseError::fail("expected retained safe packets"));
            };
            prop_assert_eq!(packet_ids(&bytes), vec![u32::MAX]);
            let snapshot = ring.snapshot();
            prop_assert_eq!(snapshot.retained_packets, capacity.min(write_count + 3));
            prop_assert!(ring.snapshot().closed.is_none());
        }

        #[test]
        fn arbitrary_aligned_write_batches_preserve_final_order(
            batch_sizes in prop::collection::vec(0_usize..20, 0..30),
        ) {
            let total = batch_sizes.iter().sum::<usize>();
            let capacity = (total + 2).max(3);
            let ring = MpegTsRing::new(MpegTsRingConfig::new(capacity, capacity).unwrap());
            ring.push(&[pat_packet().to_vec(), pmt_packet().to_vec()].concat()).unwrap();
            let mut next_id = 0_u32;
            for batch_size in batch_sizes {
                let batch = (next_id..next_id + u32::try_from(batch_size).unwrap()).collect::<Vec<_>>();
                ring.push(&packets(batch)).unwrap();
                next_id += u32::try_from(batch_size).unwrap();
                prop_assert!(ring.snapshot().closed.is_none());
            }

            let mut cursor = ring.subscribe_with_pre_roll(usize::MAX);
            let Some(RingRead::Packets { bytes, .. }) = cursor.try_next(capacity).unwrap() else {
                return Err(TestCaseError::fail("expected retained packets"));
            };
            prop_assert_eq!(packet_ids(&bytes), (0..next_id).collect::<Vec<_>>());
        }
    }
}
