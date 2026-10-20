//! Program Map Table (PMT) parsing.

use crate::PsiError;
use crate::descriptor::{Descriptor, parse_descriptors};
use crate::header::{CrcCheck, SectionHeader, be16, parse_header, verify_crc};

/// PMT `table_id`.
pub const PMT_TABLE_ID: u8 = 0x02;

/// One elementary stream entry from a PMT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PmtStream {
    /// Stream type from the PMT.
    pub stream_type: u8,
    /// Elementary PID.
    pub elementary_pid: u16,
    /// ES descriptors; empty for streams the C++ parser does not classify.
    pub descriptors: Vec<Descriptor>,
    /// Whether the C++ parser classifies this stream as audio.
    pub is_audio: bool,
    /// Whether the C++ parser classifies this stream as video.
    pub is_video: bool,
}

/// Parsed PMT section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pmt {
    /// Common section header.
    pub header: SectionHeader,
    /// PCR PID, zeroed when it names an elementary stream (C++ behaviour).
    pub pcr_pid: u16,
    /// Program-level descriptors.
    pub program_info: Vec<Descriptor>,
    /// Elementary streams in section order, plus an independent PCR entry.
    pub streams: Vec<PmtStream>,
}

impl Pmt {
    /// Parses a PMT section; CRC checking follows `crc`.
    pub fn parse(section: &[u8], crc: CrcCheck) -> Result<Self, PsiError> {
        let header = parse_header(section)?;
        if header.table_id != PMT_TABLE_ID {
            return Err(PsiError::BadTableId {
                expected: PMT_TABLE_ID,
                found: header.table_id,
            });
        }
        verify_crc(section, &header, crc)?;

        let end = header.payload_end();
        let mut pcr_pid = be16(section, 8)? & 0x1FFF;
        let program_info_length = (be16(section, 10)? & 0x0FFF) as usize;
        let es_start = 12usize
            .checked_add(program_info_length)
            .ok_or(PsiError::Truncated)?;
        if es_start > end {
            return Err(PsiError::Truncated);
        }
        let program_info = parse_descriptors(&section[12..es_start])?;

        let mut streams = Vec::new();
        let mut position = es_start;
        while position < end {
            if end - position < 5 {
                return Err(PsiError::Truncated);
            }
            let stream_type = section[position];
            let elementary_pid =
                (((section[position + 1] & 0x1F) as u16) << 8) | section[position + 2] as u16;
            let es_length =
                (((section[position + 3] & 0x0F) as usize) << 8) | section[position + 4] as usize;
            let descriptors_start = position + 5;
            let descriptors_end = descriptors_start
                .checked_add(es_length)
                .ok_or(PsiError::Truncated)?;
            if descriptors_end > end {
                return Err(PsiError::Truncated);
            }
            let descriptor_bytes = &section[descriptors_start..descriptors_end];

            let is_ac3 =
                stream_type == 129 || (stream_type == 6 && has_ac3_descriptor(descriptor_bytes));
            let is_video = is_video_stream_type(stream_type);
            let is_audio = is_audio_stream_type(stream_type) || is_ac3;
            if pcr_pid == elementary_pid {
                pcr_pid = 0;
            }
            // The C++ parser stores ES descriptors only for audio/video streams.
            let descriptors = if is_audio || is_video {
                parse_descriptors(descriptor_bytes)?
            } else {
                Vec::new()
            };
            streams.push(PmtStream {
                stream_type,
                elementary_pid,
                descriptors,
                is_audio,
                is_video,
            });
            position = descriptors_end;
        }

        // An independent PCR PID becomes its own stream entry, like C++ does.
        if (1..8191).contains(&pcr_pid) {
            streams.push(PmtStream {
                stream_type: 0,
                elementary_pid: pcr_pid,
                descriptors: Vec::new(),
                is_audio: false,
                is_video: false,
            });
        }

        Ok(Self {
            header,
            pcr_pid,
            program_info,
            streams,
        })
    }
}

/// Whether the C++ parser classifies a stream type as video (2, 15, 27, 36).
pub fn is_video_stream_type(stream_type: u8) -> bool {
    matches!(stream_type, 2 | 15 | 27 | 36)
}

/// Whether the C++ parser classifies a stream type as audio (3, 4, 17, 129).
pub fn is_audio_stream_type(stream_type: u8) -> bool {
    matches!(stream_type, 3 | 4 | 17 | 129)
}

/// Mirrors the C++ `is_ac3_es` scan for AC-3/E-AC-3 descriptor tags.
fn has_ac3_descriptor(descriptors: &[u8]) -> bool {
    let mut index = 0;
    while index + 1 < descriptors.len() {
        let tag = descriptors[index];
        let length = descriptors[index + 1] as usize;
        if tag == 0x6A || tag == 0x7A {
            return true;
        }
        index += 2 + length;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_section;

    fn pmt_section() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0xE100u16.to_be_bytes()); // PCR PID 0x100
        payload.extend_from_slice(&0xF006u16.to_be_bytes()); // program_info_length
        payload.extend_from_slice(&[0x09, 0x04, 0x0B, 0x00, 0x05, 0x73]);
        payload.extend_from_slice(&[0x02, 0xE1, 0x01, 0xF0, 0x00]); // video 0x101
        payload.extend_from_slice(&[0x03, 0xE1, 0x02, 0xF0, 0x00]); // audio 0x102
        build_section(PMT_TABLE_ID, 0x0064, 1, &payload)
    }

    #[test]
    fn parses_valid_section() {
        let pmt = Pmt::parse(&pmt_section(), CrcCheck::Verify).unwrap();
        assert_eq!(pmt.header.table_id, PMT_TABLE_ID);
        assert_eq!(pmt.header.table_id_extension, 100);
        assert_eq!(pmt.header.version_number, 1);
        assert_eq!(pmt.pcr_pid, 0x100);
        assert_eq!(pmt.program_info.len(), 1);
        assert!(pmt.program_info[0].is_ca());
        assert_eq!(pmt.program_info[0].ca_system_id(), Some(0x0B00));
        assert_eq!(pmt.program_info[0].ca_pid(), Some(0x0573));

        assert_eq!(pmt.streams.len(), 3);
        assert_eq!(pmt.streams[0].stream_type, 2);
        assert_eq!(pmt.streams[0].elementary_pid, 0x101);
        assert!(pmt.streams[0].is_video);
        assert!(!pmt.streams[0].is_audio);
        assert_eq!(pmt.streams[1].stream_type, 3);
        assert_eq!(pmt.streams[1].elementary_pid, 0x102);
        assert!(pmt.streams[1].is_audio);
        // Independent PCR PID is appended as a type 0 stream.
        assert_eq!(pmt.streams[2].stream_type, 0);
        assert_eq!(pmt.streams[2].elementary_pid, 0x100);
    }

    #[test]
    fn zeroes_pcr_pid_that_names_a_stream() {
        let payload = [
            0xE1, 0x01, // PCR PID 0x101
            0xF0, 0x00, // no program info
            0x02, 0xE1, 0x01, 0xF0, 0x00, // video 0x101
        ];
        let section = build_section(PMT_TABLE_ID, 7, 0, &payload);
        let pmt = Pmt::parse(&section, CrcCheck::Verify).unwrap();
        assert_eq!(pmt.pcr_pid, 0);
        assert_eq!(pmt.streams.len(), 1);
    }

    #[test]
    fn detects_ac3_descriptor_on_private_stream() {
        let payload = [
            0xE1, 0x00, // PCR PID 0x100
            0xF0, 0x00, // no program info
            0x06, 0xE1, 0x01, 0xF0, 0x02, // type 6
            0x6A, 0x00, // AC-3 descriptor
        ];
        let section = build_section(PMT_TABLE_ID, 7, 0, &payload);
        let pmt = Pmt::parse(&section, CrcCheck::Verify).unwrap();
        assert_eq!(pmt.streams[0].stream_type, 6);
        assert!(pmt.streams[0].is_audio);
        assert!(!pmt.streams[0].is_video);
        assert_eq!(pmt.streams[0].descriptors.len(), 1);
    }

    #[test]
    fn classifies_stream_types() {
        assert!(is_video_stream_type(2));
        assert!(is_video_stream_type(15));
        assert!(is_video_stream_type(27));
        assert!(is_video_stream_type(36));
        assert!(!is_video_stream_type(3));
        assert!(is_audio_stream_type(3));
        assert!(is_audio_stream_type(4));
        assert!(is_audio_stream_type(17));
        assert!(is_audio_stream_type(129));
        assert!(!is_audio_stream_type(2));
    }

    #[test]
    fn rejects_truncated_section() {
        let mut section = pmt_section();
        section.pop();
        assert_eq!(
            Pmt::parse(&section, CrcCheck::Skip),
            Err(PsiError::Truncated)
        );
    }

    #[test]
    fn rejects_bad_crc() {
        let mut section = pmt_section();
        let last = section.len() - 1;
        section[last] ^= 0xFF;
        assert_eq!(
            Pmt::parse(&section, CrcCheck::Verify),
            Err(PsiError::CrcMismatch)
        );
        assert!(Pmt::parse(&section, CrcCheck::Skip).is_ok());
    }
}
