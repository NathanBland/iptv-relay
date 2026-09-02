//! Bounded MPEG-TS PSI inspection used to find safe PAT/PMT restart points.

use std::mem;

use crate::MPEG_TS_PACKET_SIZE;

const PAT_PID: u16 = 0;
const PAT_TABLE_ID: u8 = 0x00;
const PMT_TABLE_ID: u8 = 0x02;
const MAX_SECTION_BYTES: usize = 1_024;

#[derive(Debug, Default)]
pub(crate) struct PatPmtTracker {
    pat: SectionAssembler,
    candidate: Option<PmtCandidate>,
    last_pmt_section: Option<Vec<u8>>,
    last_program_number: Option<u16>,
}

#[derive(Debug)]
struct PmtCandidate {
    program_number: u16,
    pid: u16,
    pat_start_sequence: u64,
    assembler: SectionAssembler,
}

impl PatPmtTracker {
    /// Observes one complete transport packet and returns the PAT packet's
    /// sequence once that PAT's referenced PMT has also been validated.
    pub(crate) fn observe(&mut self, packet: &[u8], sequence: u64) -> Option<u64> {
        let header = packet_header(packet)?;

        if header.pid == PAT_PID
            && let Some(section) = self.pat.observe(packet, header, sequence)
            && let Some((program_number, pid)) = parse_pat(&section.bytes)
        {
            self.candidate = Some(PmtCandidate {
                program_number,
                pid,
                pat_start_sequence: section.start_sequence,
                assembler: SectionAssembler::default(),
            });
        }

        let candidate = self.candidate.as_mut()?;
        if header.pid != candidate.pid {
            return None;
        }
        let section = candidate.assembler.observe(packet, header, sequence)?;
        if !is_matching_pmt(&section.bytes, candidate.program_number) {
            return None;
        }
        let boundary = candidate.pat_start_sequence;
        self.last_pmt_section = Some(section.bytes.clone());
        self.last_program_number = Some(candidate.program_number);
        self.candidate = None;
        Some(boundary)
    }

    pub(crate) fn candidate_start(&self) -> Option<u64> {
        self.candidate
            .as_ref()
            .map(|candidate| candidate.pat_start_sequence)
    }

    /// Returns the most recently validated PMT section and its program number.
    pub(crate) fn completed_pmt(&self) -> Option<(&[u8], u16)> {
        self.last_pmt_section
            .as_deref()
            .zip(self.last_program_number)
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug)]
struct PacketHeader<'a> {
    pid: u16,
    payload_unit_start: bool,
    continuity_counter: u8,
    payload: &'a [u8],
}

fn packet_header(packet: &[u8]) -> Option<PacketHeader<'_>> {
    if packet.len() != MPEG_TS_PACKET_SIZE
        || packet[0] != 0x47
        || packet[1] & 0x80 != 0
        || packet[3] & 0xc0 != 0
    {
        return None;
    }
    let adaptation_control = (packet[3] >> 4) & 0x03;
    if adaptation_control == 0 || adaptation_control == 2 {
        return None;
    }
    let payload_offset = if adaptation_control == 3 {
        5_usize.checked_add(usize::from(packet[4]))?
    } else {
        4
    };
    if payload_offset > packet.len() {
        return None;
    }
    Some(PacketHeader {
        pid: (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]),
        payload_unit_start: packet[1] & 0x40 != 0,
        continuity_counter: packet[3] & 0x0f,
        payload: &packet[payload_offset..],
    })
}

#[derive(Debug, Default)]
struct SectionAssembler {
    bytes: Vec<u8>,
    expected_len: Option<usize>,
    start_sequence: Option<u64>,
    last_continuity_counter: Option<u8>,
}

#[derive(Debug)]
struct CompletedSection {
    bytes: Vec<u8>,
    start_sequence: u64,
}

impl SectionAssembler {
    fn observe(
        &mut self,
        _packet: &[u8],
        header: PacketHeader<'_>,
        sequence: u64,
    ) -> Option<CompletedSection> {
        let payload = if header.payload_unit_start {
            let Some((&pointer, rest)) = header.payload.split_first() else {
                self.clear();
                return None;
            };
            let start = usize::from(pointer);
            if start > rest.len() {
                self.clear();
                return None;
            }
            self.clear();
            self.start_sequence = Some(sequence);
            self.last_continuity_counter = Some(header.continuity_counter);
            &rest[start..]
        } else {
            self.start_sequence?;
            if self
                .last_continuity_counter
                .is_none_or(|previous| (previous + 1) & 0x0f != header.continuity_counter)
            {
                self.clear();
                return None;
            }
            self.last_continuity_counter = Some(header.continuity_counter);
            header.payload
        };

        let remaining = self
            .expected_len
            .map_or(MAX_SECTION_BYTES, |expected| expected - self.bytes.len());
        self.bytes
            .extend_from_slice(&payload[..payload.len().min(remaining)]);
        if self.expected_len.is_none() && self.bytes.len() >= 3 {
            let section_length =
                (usize::from(self.bytes[1] & 0x0f) << 8) | usize::from(self.bytes[2]);
            let expected = 3_usize.checked_add(section_length)?;
            if !(4..=MAX_SECTION_BYTES).contains(&expected) {
                self.clear();
                return None;
            }
            self.expected_len = Some(expected);
            self.bytes.truncate(expected);
        }

        let expected = self.expected_len?;
        if self.bytes.len() < expected {
            return None;
        }
        let start_sequence = self.start_sequence?;
        let bytes = mem::take(&mut self.bytes);
        self.expected_len = None;
        self.start_sequence = None;
        self.last_continuity_counter = None;
        Some(CompletedSection {
            bytes,
            start_sequence,
        })
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.expected_len = None;
        self.start_sequence = None;
        self.last_continuity_counter = None;
    }
}

fn parse_pat(section: &[u8]) -> Option<(u16, u16)> {
    if section.len() < 16
        || section[0] != PAT_TABLE_ID
        || section[1] & 0x80 == 0
        || section[5] & 0x01 == 0
        || mpeg_crc32(section) != 0
    {
        return None;
    }
    let entries = section.get(8..section.len().checked_sub(4)?)?;
    if !entries.len().is_multiple_of(4) {
        return None;
    }
    entries.chunks_exact(4).find_map(|entry| {
        let program_number = u16::from_be_bytes([entry[0], entry[1]]);
        let pid = (u16::from(entry[2] & 0x1f) << 8) | u16::from(entry[3]);
        (program_number != 0 && pid != 0 && pid != 0x1fff).then_some((program_number, pid))
    })
}

/// One elementary stream declared inside a PMT section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ElementaryStreamInfo {
    pub stream_type: u8,
    pub pid: u16,
}

/// Parses a validated PMT section and returns its elementary stream entries.
///
/// Returns `None` when the section is not a valid PMT for `expected_program`.
/// The caller validates the CRC separately, so this function trusts the bytes.
pub(crate) fn parse_pmt_elementary_streams(
    section: &[u8],
    expected_program: u16,
) -> Option<Vec<ElementaryStreamInfo>> {
    if section.len() < 16
        || section[0] != PMT_TABLE_ID
        || section[1] & 0x80 == 0
        || section[5] & 0x01 == 0
        || u16::from_be_bytes([section[3], section[4]]) != expected_program
    {
        return None;
    }
    let program_info_length = (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
    let mut offset = 12_usize.checked_add(program_info_length)?;
    let streams_end = section.len().saturating_sub(4);
    if offset > streams_end {
        return None;
    }
    let mut streams = Vec::new();
    while offset < streams_end {
        let header = section.get(offset..offset.saturating_add(5))?;
        let stream_type = header[0];
        let pid = (u16::from(header[1] & 0x1f) << 8) | u16::from(header[2]);
        let es_info_length = (usize::from(header[3] & 0x0f) << 8) | usize::from(header[4]);
        let next = offset
            .checked_add(5)
            .and_then(|offset| offset.checked_add(es_info_length))?;
        if next > streams_end {
            return None;
        }
        streams.push(ElementaryStreamInfo { stream_type, pid });
        offset = next;
    }
    if offset != streams_end {
        return None;
    }
    Some(streams)
}

fn is_matching_pmt(section: &[u8], expected_program: u16) -> bool {
    if section.len() < 16
        || section[0] != PMT_TABLE_ID
        || section[1] & 0x80 == 0
        || section[5] & 0x01 == 0
        || u16::from_be_bytes([section[3], section[4]]) != expected_program
        || mpeg_crc32(section) != 0
    {
        return false;
    }
    let program_info_length = (usize::from(section[10] & 0x0f) << 8) | usize::from(section[11]);
    let Some(mut offset) = 12_usize.checked_add(program_info_length) else {
        return false;
    };
    let streams_end = section.len().saturating_sub(4);
    if offset > streams_end {
        return false;
    }
    while offset < streams_end {
        let Some(header) = section.get(offset..offset.saturating_add(5)) else {
            return false;
        };
        let es_info_length = (usize::from(header[3] & 0x0f) << 8) | usize::from(header[4]);
        let Some(next) = offset
            .checked_add(5)
            .and_then(|offset| offset.checked_add(es_info_length))
        else {
            return false;
        };
        if next > streams_end {
            return false;
        }
        offset = next;
    }
    offset == streams_end
}

fn mpeg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) const TEST_PMT_PID: u16 = 0x100;
    pub(crate) const TEST_VIDEO_PID: u16 = 0x101;

    pub(crate) fn pat_packet() -> [u8; MPEG_TS_PACKET_SIZE] {
        psi_packet(PAT_PID, &pat_section(TEST_PMT_PID), 0, 0)
    }

    pub(crate) fn pmt_packet() -> [u8; MPEG_TS_PACKET_SIZE] {
        psi_packet(TEST_PMT_PID, &pmt_section(TEST_VIDEO_PID), 0, 0)
    }

    pub(crate) fn psi_packet(
        pid: u16,
        section: &[u8],
        adaptation_length: usize,
        pointer: usize,
    ) -> [u8; MPEG_TS_PACKET_SIZE] {
        let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = 0x40 | u8::try_from((pid >> 8) & 0x1f).unwrap();
        packet[2] = u8::try_from(pid & 0xff).unwrap();
        let mut offset = 4;
        if adaptation_length > 0 {
            packet[3] = 0x30;
            packet[4] = u8::try_from(adaptation_length).unwrap();
            offset += adaptation_length + 1;
        } else {
            packet[3] = 0x10;
        }
        packet[offset] = u8::try_from(pointer).unwrap();
        offset += 1 + pointer;
        packet[offset..offset + section.len()].copy_from_slice(section);
        packet
    }

    pub(crate) fn pat_section(pmt_pid: u16) -> Vec<u8> {
        let mut section = vec![
            PAT_TABLE_ID,
            0xb0,
            0x0d,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0x00,
            0x01,
            0xe0 | u8::try_from((pmt_pid >> 8) & 0x1f).unwrap(),
            u8::try_from(pmt_pid & 0xff).unwrap(),
        ];
        append_crc(&mut section);
        section
    }

    pub(crate) fn pmt_section(video_pid: u16) -> Vec<u8> {
        pmt_section_with_streams(video_pid, &[(0x1b, video_pid)])
    }

    /// Builds a PMT section that declares one video and one audio elementary
    /// stream. The audio PID is distinct from the video PID.
    pub(crate) fn pmt_section_with_audio(video_pid: u16, audio_pid: u16) -> Vec<u8> {
        pmt_section_with_streams(video_pid, &[(0x1b, video_pid), (0x04, audio_pid)])
    }

    /// Builds a PMT section with a custom set of elementary streams. The first
    /// PID is used as the PCR PID. Each entry is `(stream_type, pid)`.
    pub(crate) fn pmt_section_with_streams(pcr_pid: u16, streams: &[(u8, u16)]) -> Vec<u8> {
        let mut section = vec![
            PMT_TABLE_ID,
            0xb0, // section_syntax_indicator + reserved + section_length[11:8]
            0x00, // section_length[7:0] (filled below)
            0x00, // program_number high
            0x01, // program_number low
            0xc1, // reserved + version + current_next
            0x00, // section_number
            0x00, // last_section_number
            0xe0 | u8::try_from((pcr_pid >> 8) & 0x1f).unwrap(),
            u8::try_from(pcr_pid & 0xff).unwrap(),
            0xf0, // reserved + program_info_length high
            0x00, // program_info_length low
        ];
        for (stream_type, pid) in streams {
            section.push(*stream_type);
            section.push(0xe0 | u8::try_from((pid >> 8) & 0x1f).unwrap());
            section.push(u8::try_from(pid & 0xff).unwrap());
            section.push(0xf0); // reserved + ES info length high
            section.push(0x00); // ES info length low
        }
        // Compute and set the section_length (bytes after this field, including CRC).
        let section_length = u16::try_from(section.len() - 3 + 4).unwrap();
        section[1] |= u8::try_from((section_length >> 8) & 0x0f).unwrap();
        section[2] = u8::try_from(section_length & 0xff).unwrap();
        append_crc(&mut section);
        section
    }

    fn append_crc(section: &mut Vec<u8>) {
        let crc = mpeg_crc32(section);
        section.extend_from_slice(&crc.to_be_bytes());
        assert_eq!(mpeg_crc32(section), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::*, *};

    #[test]
    fn validates_pat_pmt_with_adaptation_and_pointer_fields() {
        let pat = psi_packet(PAT_PID, &pat_section(TEST_PMT_PID), 3, 2);
        let pmt = psi_packet(TEST_PMT_PID, &pmt_section(TEST_VIDEO_PID), 5, 1);
        let mut tracker = PatPmtTracker::default();
        assert_eq!(tracker.observe(&pat, 9), None);
        assert_eq!(tracker.candidate_start(), Some(9));
        assert_eq!(tracker.observe(&pmt, 10), Some(9));
        assert_eq!(tracker.candidate_start(), None);
    }

    #[test]
    fn rejects_bad_crc_pointer_adaptation_and_wrong_program() {
        let mut tracker = PatPmtTracker::default();
        let mut bad_crc = pat_packet();
        bad_crc[10] ^= 1;
        assert_eq!(tracker.observe(&bad_crc, 0), None);
        assert_eq!(tracker.candidate_start(), None);

        let mut bad_pointer = pat_packet();
        bad_pointer[4] = u8::MAX;
        assert_eq!(tracker.observe(&bad_pointer, 1), None);

        let mut bad_adaptation = pat_packet();
        bad_adaptation[3] = 0x30;
        bad_adaptation[4] = u8::MAX;
        assert_eq!(tracker.observe(&bad_adaptation, 2), None);

        assert_eq!(tracker.observe(&pat_packet(), 3), None);
        let mut wrong_program = pmt_section(TEST_VIDEO_PID);
        wrong_program[4] = 2;
        let crc_offset = wrong_program.len() - 4;
        wrong_program.truncate(crc_offset);
        append_test_crc(&mut wrong_program);
        let wrong_program = psi_packet(TEST_PMT_PID, &wrong_program, 0, 0);
        assert_eq!(tracker.observe(&wrong_program, 4), None);
    }

    #[test]
    fn assembles_a_bounded_pat_section_across_transport_packets() {
        let section = pat_section(TEST_PMT_PID);
        let split = 8;
        let mut first = [0xff; MPEG_TS_PACKET_SIZE];
        first[0] = 0x47;
        first[1] = 0x40;
        first[2] = 0;
        first[3] = 0x30;
        let adaptation_length = MPEG_TS_PACKET_SIZE - 5 - 1 - split;
        first[4] = u8::try_from(adaptation_length).unwrap();
        let pointer_offset = 5 + adaptation_length;
        first[pointer_offset] = 0;
        first[pointer_offset + 1..].copy_from_slice(&section[..split]);

        let mut continuation = [0xff; MPEG_TS_PACKET_SIZE];
        continuation[0] = 0x47;
        continuation[1] = 0;
        continuation[2] = 0;
        continuation[3] = 0x11;
        continuation[4..4 + section.len() - split].copy_from_slice(&section[split..]);

        let mut tracker = PatPmtTracker::default();
        assert_eq!(tracker.observe(&first, 20), None);
        assert_eq!(tracker.candidate_start(), None);
        assert_eq!(tracker.observe(&continuation, 21), None);
        assert_eq!(tracker.candidate_start(), Some(20));
        assert_eq!(tracker.observe(&pmt_packet(), 22), Some(20));
    }

    fn append_test_crc(section: &mut Vec<u8>) {
        let crc = mpeg_crc32(section);
        section.extend_from_slice(&crc.to_be_bytes());
    }

    #[test]
    fn completed_pmt_exposes_elementary_streams_for_audio_and_video() {
        const TEST_AUDIO_PID: u16 = 0x102;
        let pat = pat_packet();
        let pmt = psi_packet(
            TEST_PMT_PID,
            &pmt_section_with_audio(TEST_VIDEO_PID, TEST_AUDIO_PID),
            0,
            0,
        );
        let mut tracker = PatPmtTracker::default();
        assert_eq!(tracker.observe(&pat, 0), None);
        assert_eq!(tracker.observe(&pmt, 1), Some(0));

        let (section, program) = tracker
            .completed_pmt()
            .expect("completed PMT section is retained");
        assert_eq!(program, 1);
        let streams = parse_pmt_elementary_streams(section, program)
            .expect("elementary streams parse from a valid PMT");
        assert_eq!(streams.len(), 2);
        assert_eq!(
            streams[0],
            ElementaryStreamInfo {
                stream_type: 0x1b,
                pid: TEST_VIDEO_PID
            }
        );
        assert_eq!(
            streams[1],
            ElementaryStreamInfo {
                stream_type: 0x04,
                pid: TEST_AUDIO_PID
            }
        );
    }

    #[test]
    fn parse_pmt_elementary_streams_rejects_wrong_program_and_bad_sections() {
        assert!(parse_pmt_elementary_streams(&pmt_section(TEST_VIDEO_PID), 999).is_none());
        assert!(parse_pmt_elementary_streams(&[], 1).is_none());
        assert!(parse_pmt_elementary_streams(&pat_section(TEST_PMT_PID), 1).is_none());
    }

    #[test]
    fn parse_pmt_elementary_streams_rejects_truncated_stream_loop() {
        // Build a PMT section where the program_info_length pushes the offset
        // past the streams end, so the stream loop cannot start.
        let mut section = pmt_section_with_streams(TEST_VIDEO_PID, &[(0x1b, TEST_VIDEO_PID)]);
        // Set program_info_length to a value larger than the remaining bytes.
        section[10] = 0xf0;
        section[11] = 0x80; // program_info_length = 128, far past the section end
        assert!(parse_pmt_elementary_streams(&section, 1).is_none());
    }

    #[test]
    fn parse_pmt_elementary_streams_rejects_oversized_es_info_length() {
        // Build a PMT section where one elementary stream declares an
        // ES info length that extends past the streams end.
        let mut section = pmt_section_with_streams(TEST_VIDEO_PID, &[(0x1b, TEST_VIDEO_PID)]);
        // Overwrite the ES info length of the first (and only) stream entry
        // to push `next` past `streams_end`. The stream entry starts at offset
        // 12 (after program_info_length of 0). Bytes 12..17 are the 5-byte
        // stream header. Byte 16 is ES info length high, byte 17 is low.
        section[16] = 0x0f;
        section[17] = 0xff; // ES info length = 4095, far past the section end
        assert!(parse_pmt_elementary_streams(&section, 1).is_none());
    }

    #[test]
    fn parse_pmt_elementary_streams_rejects_trailing_bytes_after_stream_loop() {
        // Build a PMT section with an extra byte after the stream loop so the
        // final offset != streams_end check fails.
        let mut section = pmt_section_with_streams(TEST_VIDEO_PID, &[(0x1b, TEST_VIDEO_PID)]);
        // Insert one extra byte before the CRC to misalign the stream loop end.
        let crc = section.split_off(section.len() - 4);
        section.push(0x00); // extra byte
        section.extend_from_slice(&crc);
        assert!(parse_pmt_elementary_streams(&section, 1).is_none());
    }
}
