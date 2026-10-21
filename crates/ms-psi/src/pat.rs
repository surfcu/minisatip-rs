//! Program Association Table (PAT) parsing.

use crate::PsiError;
use crate::header::{CrcCheck, SectionHeader, parse_header, verify_crc};

/// PAT `table_id`.
pub const PAT_TABLE_ID: u8 = 0x00;

/// One program association: a program number and its PMT (or NIT) PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatEntry {
    /// Program number; zero denotes the NIT.
    pub program_number: u16,
    /// PID carrying the PMT for this program, or the NIT PID for program 0.
    pub pid: u16,
}

/// Parsed PAT section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pat {
    /// Common section header.
    pub header: SectionHeader,
    /// Associations in section order, including the program 0 NIT entry.
    pub entries: Vec<PatEntry>,
}

impl Pat {
    /// Parses a PAT section; CRC checking follows `crc`.
    pub fn parse(section: &[u8], crc: CrcCheck) -> Result<Self, PsiError> {
        let header = parse_header(section)?;
        if header.table_id != PAT_TABLE_ID {
            return Err(PsiError::BadTableId {
                expected: PAT_TABLE_ID,
                found: header.table_id,
            });
        }
        verify_crc(section, &header, crc)?;
        let payload = section
            .get(8..header.payload_end())
            .ok_or(PsiError::Truncated)?;
        let mut entries = Vec::with_capacity(payload.len() / 4);
        for entry in payload.as_chunks::<4>().0 {
            entries.push(PatEntry {
                program_number: u16::from_be_bytes([entry[0], entry[1]]),
                pid: (((entry[2] & 0x1F) as u16) << 8) | entry[3] as u16,
            });
        }
        Ok(Self { header, entries })
    }

    /// PID carrying the NIT (the program number 0 entry), when present.
    pub fn nit_pid(&self) -> Option<u16> {
        self.entries
            .iter()
            .find(|entry| entry.program_number == 0)
            .map(|entry| entry.pid)
    }

    /// Entries for real programs, excluding the NIT entry.
    pub fn programs(&self) -> impl Iterator<Item = &PatEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.program_number != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_section;

    fn pat_section() -> Vec<u8> {
        let payload = [
            0x00, 0x01, 0xE1, 0x00, // program 1 -> PID 0x100
            0x00, 0x00, 0xE0, 0x10, // NIT -> PID 0x10
        ];
        build_section(PAT_TABLE_ID, 0x1234, 3, &payload)
    }

    #[test]
    fn parses_valid_section() {
        let pat = Pat::parse(&pat_section(), CrcCheck::Verify).unwrap();
        assert_eq!(pat.header.table_id, PAT_TABLE_ID);
        assert_eq!(pat.header.table_id_extension, 0x1234);
        assert_eq!(pat.header.version_number, 3);
        assert_eq!(
            pat.entries,
            [
                PatEntry {
                    program_number: 1,
                    pid: 0x100
                },
                PatEntry {
                    program_number: 0,
                    pid: 0x10
                },
            ]
        );
        assert_eq!(pat.nit_pid(), Some(0x10));
        assert_eq!(
            pat.programs().copied().collect::<Vec<_>>(),
            [PatEntry {
                program_number: 1,
                pid: 0x100
            }]
        );
    }

    #[test]
    fn rejects_truncated_section() {
        let mut section = pat_section();
        section.pop();
        assert_eq!(
            Pat::parse(&section, CrcCheck::Skip),
            Err(PsiError::Truncated)
        );
    }

    #[test]
    fn rejects_bad_crc() {
        let mut section = pat_section();
        let last = section.len() - 1;
        section[last] ^= 0xFF;
        assert_eq!(
            Pat::parse(&section, CrcCheck::Verify),
            Err(PsiError::CrcMismatch)
        );
        assert!(Pat::parse(&section, CrcCheck::Skip).is_ok());
    }
}
