//! Conditional Access Table (CAT) parsing.

use crate::PsiError;
use crate::descriptor::{Descriptor, parse_descriptors};
use crate::header::{CrcCheck, SectionHeader, parse_header, verify_crc};

/// CAT `table_id`.
pub const CAT_TABLE_ID: u8 = 0x01;

/// Parsed CAT section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cat {
    /// Common section header.
    pub header: SectionHeader,
    /// Descriptors carried by the table, typically CA descriptors.
    pub descriptors: Vec<Descriptor>,
}

impl Cat {
    /// Parses a CAT section; CRC checking follows `crc`.
    pub fn parse(section: &[u8], crc: CrcCheck) -> Result<Self, PsiError> {
        let header = parse_header(section)?;
        if header.table_id != CAT_TABLE_ID {
            return Err(PsiError::BadTableId {
                expected: CAT_TABLE_ID,
                found: header.table_id,
            });
        }
        verify_crc(section, &header, crc)?;
        let payload = section
            .get(8..header.payload_end())
            .ok_or(PsiError::Truncated)?;
        let descriptors = parse_descriptors(payload)?;
        Ok(Self {
            header,
            descriptors,
        })
    }

    /// CA descriptors (tag 0x09) carried by the table.
    pub fn ca_descriptors(&self) -> impl Iterator<Item = &Descriptor> {
        self.descriptors
            .iter()
            .filter(|descriptor| descriptor.is_ca())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_section;

    fn cat_section() -> Vec<u8> {
        build_section(CAT_TABLE_ID, 0, 0, &[0x09, 0x04, 0x0B, 0x00, 0x05, 0x73])
    }

    #[test]
    fn parses_valid_section() {
        let cat = Cat::parse(&cat_section(), CrcCheck::Verify).unwrap();
        assert_eq!(cat.header.table_id, CAT_TABLE_ID);
        assert_eq!(cat.descriptors.len(), 1);
        assert_eq!(cat.ca_descriptors().count(), 1);
        let descriptor = &cat.descriptors[0];
        assert!(descriptor.is_ca());
        assert_eq!(descriptor.ca_system_id(), Some(0x0B00));
        assert_eq!(descriptor.ca_pid(), Some(0x0573));
    }

    #[test]
    fn rejects_truncated_section() {
        let mut section = cat_section();
        section.pop();
        assert_eq!(
            Cat::parse(&section, CrcCheck::Skip),
            Err(PsiError::Truncated)
        );
    }

    #[test]
    fn rejects_bad_crc() {
        let mut section = cat_section();
        let last = section.len() - 1;
        section[last] ^= 0xFF;
        assert_eq!(
            Cat::parse(&section, CrcCheck::Verify),
            Err(PsiError::CrcMismatch)
        );
        assert!(Cat::parse(&section, CrcCheck::Skip).is_ok());
    }
}
