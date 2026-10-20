//! Common long-form PSI section header.

use ms_ts::crc32_mpeg2;

use crate::PsiError;

/// Whether a parser verifies the trailing CRC, mirroring C++ `FILTER_CRC`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CrcCheck {
    /// Do not verify; the C++ parser functions also trust their input.
    #[default]
    Skip,
    /// Verify the trailing MPEG-2 CRC-32.
    Verify,
}

/// Fields shared by long-form PSI sections (PAT, PMT, CAT, SDT).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionHeader {
    /// Table identifier.
    pub table_id: u8,
    /// Section syntax indicator bit.
    pub section_syntax_indicator: bool,
    /// Declared number of bytes after the length field.
    pub section_length: u16,
    /// Table identifier extension (TSID, program number or service id).
    pub table_id_extension: u16,
    /// Version number.
    pub version_number: u8,
    /// Whether this section applies to the current (not next) table.
    pub current_next: bool,
    /// Section index within the table.
    pub section_number: u8,
    /// Index of the last section in the table.
    pub last_section_number: u8,
}

impl SectionHeader {
    /// Total size of the section, including the three-byte header.
    pub fn total_len(&self) -> usize {
        3 + self.section_length as usize
    }

    /// Offset of the trailing CRC, one past the table payload.
    pub fn payload_end(&self) -> usize {
        self.total_len().saturating_sub(4)
    }
}

/// Parses the common long-form header and validates the declared length.
pub fn parse_header(section: &[u8]) -> Result<SectionHeader, PsiError> {
    let bytes = section.get(..8).ok_or(PsiError::Truncated)?;
    let section_length = (((bytes[1] & 0x0F) as u16) << 8) | bytes[2] as u16;
    let total = 3 + section_length as usize;
    // Long form needs the eight header bytes plus the four CRC bytes.
    if section_length < 9 || total > section.len() {
        return Err(PsiError::Truncated);
    }
    Ok(SectionHeader {
        table_id: bytes[0],
        section_syntax_indicator: bytes[1] & 0x80 != 0,
        section_length,
        table_id_extension: u16::from_be_bytes([bytes[3], bytes[4]]),
        version_number: (bytes[5] & 0x3E) >> 1,
        current_next: bytes[5] & 0x01 != 0,
        section_number: bytes[6],
        last_section_number: bytes[7],
    })
}

pub(crate) fn verify_crc(
    section: &[u8],
    header: &SectionHeader,
    check: CrcCheck,
) -> Result<(), PsiError> {
    if check == CrcCheck::Skip {
        return Ok(());
    }
    let end = header.total_len();
    let tail = section.get(end - 4..end).ok_or(PsiError::Truncated)?;
    let stored = u32::from_be_bytes(tail.try_into().map_err(|_| PsiError::Truncated)?);
    if crc32_mpeg2(&section[..end - 4]) != stored {
        return Err(PsiError::CrcMismatch);
    }
    Ok(())
}

pub(crate) fn be16(bytes: &[u8], offset: usize) -> Result<u16, PsiError> {
    let end = offset.checked_add(2).ok_or(PsiError::Truncated)?;
    let pair = bytes.get(offset..end).ok_or(PsiError::Truncated)?;
    Ok(u16::from_be_bytes([pair[0], pair[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_section;

    #[test]
    fn parses_header_fields() {
        let section = build_section(0x42, 0x1234, 5, &[0; 12]);
        let header = parse_header(&section).unwrap();
        assert_eq!(header.table_id, 0x42);
        assert!(header.section_syntax_indicator);
        assert_eq!(header.section_length, 21);
        assert_eq!(header.table_id_extension, 0x1234);
        assert_eq!(header.version_number, 5);
        assert!(header.current_next);
        assert_eq!(header.section_number, 0);
        assert_eq!(header.last_section_number, 0);
        assert_eq!(header.payload_end(), 20);
        assert_eq!(header.total_len(), section.len());
    }

    #[test]
    fn rejects_short_and_overlong_sections() {
        assert_eq!(parse_header(&[]), Err(PsiError::Truncated));
        assert_eq!(parse_header(&[0; 7]), Err(PsiError::Truncated));
        let section = build_section(0x00, 0, 0, &[0; 4]);
        assert_eq!(
            parse_header(&section[..section.len() - 1]),
            Err(PsiError::Truncated)
        );
    }
}
