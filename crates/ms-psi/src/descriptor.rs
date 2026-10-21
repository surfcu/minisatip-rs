//! Generic PSI descriptor iteration and CA descriptor accessors.

use crate::PsiError;

/// A descriptor copied out of a section: tag plus the bytes after its length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Descriptor {
    /// Descriptor tag.
    pub tag: u8,
    /// Descriptor payload, excluding the tag and length bytes.
    pub data: Vec<u8>,
}

impl Descriptor {
    /// Whether this is a CA descriptor (tag 0x09).
    pub fn is_ca(&self) -> bool {
        self.tag == 0x09
    }

    /// CA_system_id (caid) from a CA descriptor payload.
    pub fn ca_system_id(&self) -> Option<u16> {
        let data = self.data.get(..2)?;
        Some(u16::from_be_bytes([data[0], data[1]]))
    }

    /// CA_PID (capid) from a CA descriptor payload.
    pub fn ca_pid(&self) -> Option<u16> {
        let data = self.data.get(..4)?;
        Some((((data[2] & 0x1F) as u16) << 8) | data[3] as u16)
    }

    /// Private data following CA_system_id and CA_PID.
    pub fn private_data(&self) -> &[u8] {
        self.data.get(4..).unwrap_or(&[])
    }
}

/// Iterator over the `(tag, bytes)` entries of a descriptor loop.
#[derive(Clone, Debug)]
pub struct DescriptorIter<'a> {
    rest: &'a [u8],
}

impl Iterator for DescriptorIter<'_> {
    type Item = Result<Descriptor, PsiError>;

    fn next(&mut self) -> Option<Self::Item> {
        let &tag = self.rest.first()?;
        let Some(&length) = self.rest.get(1) else {
            self.rest = &[];
            return Some(Err(PsiError::Truncated));
        };
        let end = 2 + length as usize;
        let Some(data) = self.rest.get(2..end) else {
            self.rest = &[];
            return Some(Err(PsiError::Truncated));
        };
        self.rest = &self.rest[end..];
        Some(Ok(Descriptor {
            tag,
            data: data.to_vec(),
        }))
    }
}

/// Iterates a descriptor loop, yielding each descriptor or its error.
pub fn iter_descriptors(bytes: &[u8]) -> DescriptorIter<'_> {
    DescriptorIter { rest: bytes }
}

/// Parses a complete descriptor loop.
pub fn parse_descriptors(bytes: &[u8]) -> Result<Vec<Descriptor>, PsiError> {
    iter_descriptors(bytes).collect()
}

/// Parses exactly one descriptor from the start of `bytes`.
pub fn parse_descriptor(bytes: &[u8]) -> Result<Descriptor, PsiError> {
    iter_descriptors(bytes)
        .next()
        .unwrap_or(Err(PsiError::Truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterates_tag_and_bytes() {
        let descriptors =
            parse_descriptors(&[0x09, 0x04, 0x0B, 0x00, 0x05, 0x73, 0x48, 0x00]).unwrap();
        assert_eq!(descriptors.len(), 2);
        assert_eq!(descriptors[0].tag, 0x09);
        assert_eq!(descriptors[0].data, [0x0B, 0x00, 0x05, 0x73]);
        assert!(descriptors[0].is_ca());
        assert_eq!(descriptors[0].ca_system_id(), Some(0x0B00));
        assert_eq!(descriptors[0].ca_pid(), Some(0x0573));
        assert_eq!(descriptors[0].private_data(), &[]);
        assert_eq!(descriptors[1].tag, 0x48);
        assert!(!descriptors[1].is_ca());
    }

    #[test]
    fn short_descriptors_report_errors() {
        assert_eq!(parse_descriptors(&[0x09]), Err(PsiError::Truncated));
        assert_eq!(
            parse_descriptors(&[0x09, 0x04, 0x00]),
            Err(PsiError::Truncated)
        );
        let short = Descriptor {
            tag: 0x09,
            data: vec![0x0B, 0x00],
        };
        assert_eq!(short.ca_system_id(), Some(0x0B00));
        assert_eq!(short.ca_pid(), None);
        assert_eq!(short.private_data(), &[]);
    }
}
