//! Service Description Table (SDT) parsing.

use crate::PsiError;
use crate::charset::{CharsetOverride, dvb_get_string};
use crate::descriptor::{Descriptor, parse_descriptors};
use crate::header::{CrcCheck, SectionHeader, be16, parse_header, verify_crc};

/// SDT `table_id` for the transport stream being received.
pub const SDT_ACTUAL_TABLE_ID: u8 = 0x42;
/// SDT `table_id` for another transport stream.
pub const SDT_OTHER_TABLE_ID: u8 = 0x46;

// C++ uses `sizeof(pmt->name) - 1` as the bound.
const NAME_LENGTH: usize = 49;

/// One service entry from an SDT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdtService {
    /// Service identifier.
    pub service_id: u16,
    /// EIT schedule flag.
    pub eit_schedule: bool,
    /// EIT present/following flag.
    pub eit_present_following: bool,
    /// Running status (0..=7).
    pub running_status: u8,
    /// Whether the service is scrambled.
    pub free_ca_mode: bool,
    /// Service descriptors in section order.
    pub descriptors: Vec<Descriptor>,
    /// Service type from the service descriptor (0x48), when present.
    pub service_type: Option<u8>,
    /// Decoded provider name from the service descriptor, when present.
    pub provider_name: Option<Vec<u8>>,
    /// Decoded service name from the service descriptor, when present.
    pub service_name: Option<Vec<u8>>,
}

/// Parsed SDT section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sdt {
    /// Common section header.
    pub header: SectionHeader,
    /// Transport stream identifier (`table_id_extension`).
    pub transport_stream_id: u16,
    /// Original network identifier.
    pub original_network_id: u16,
    /// Services in section order.
    pub services: Vec<SdtService>,
}

impl Sdt {
    /// Parses an actual or other-transport-stream SDT; CRC follows `crc`.
    pub fn parse(section: &[u8], crc: CrcCheck) -> Result<Self, PsiError> {
        let header = parse_header(section)?;
        if header.table_id != SDT_ACTUAL_TABLE_ID && header.table_id != SDT_OTHER_TABLE_ID {
            return Err(PsiError::BadTableId {
                expected: SDT_ACTUAL_TABLE_ID,
                found: header.table_id,
            });
        }
        verify_crc(section, &header, crc)?;

        let end = header.payload_end();
        let original_network_id = be16(section, 8)?;
        let mut position = 11;
        let mut services = Vec::new();
        while position < end {
            if end - position < 5 {
                return Err(PsiError::Truncated);
            }
            let service_id = be16(section, position)?;
            let flags = section[position + 2];
            let status = section[position + 3];
            let descriptors_length =
                (((status & 0x0F) as usize) << 8) | section[position + 4] as usize;
            let descriptors_start = position + 5;
            let descriptors_end = descriptors_start
                .checked_add(descriptors_length)
                .ok_or(PsiError::Truncated)?;
            if descriptors_end > end {
                return Err(PsiError::Truncated);
            }
            let descriptors = parse_descriptors(&section[descriptors_start..descriptors_end])?;
            let decoded = decode_service_descriptor(&descriptors)?;
            services.push(SdtService {
                service_id,
                eit_schedule: flags & 0x02 != 0,
                eit_present_following: flags & 0x01 != 0,
                running_status: (status >> 5) & 0x07,
                free_ca_mode: status & 0x10 != 0,
                descriptors,
                service_type: decoded.service_type,
                provider_name: decoded.provider_name,
                service_name: decoded.service_name,
            });
            position = descriptors_end;
        }

        Ok(Self {
            header,
            transport_stream_id: header.table_id_extension,
            original_network_id,
            services,
        })
    }
}

struct ServiceDescriptor {
    service_type: Option<u8>,
    provider_name: Option<Vec<u8>>,
    service_name: Option<Vec<u8>>,
}

fn decode_service_descriptor(descriptors: &[Descriptor]) -> Result<ServiceDescriptor, PsiError> {
    let mut decoded = ServiceDescriptor {
        service_type: None,
        provider_name: None,
        service_name: None,
    };
    for descriptor in descriptors.iter().filter(|item| item.tag == 0x48) {
        let (kind, rest) = descriptor
            .data
            .split_first()
            .ok_or(PsiError::BadDescriptor)?;
        decoded.service_type = Some(*kind);
        let (provider, rest) = split_dvb_string(rest)?;
        let (name, _) = split_dvb_string(rest)?;
        decoded.provider_name = dvb_get_string(provider, NAME_LENGTH, CharsetOverride::Auto);
        decoded.service_name = dvb_get_string(name, NAME_LENGTH, CharsetOverride::Auto);
    }
    Ok(decoded)
}

fn split_dvb_string(bytes: &[u8]) -> Result<(&[u8], &[u8]), PsiError> {
    let (length, rest) = bytes.split_first().ok_or(PsiError::BadDescriptor)?;
    let end = *length as usize;
    let value = rest.get(..end).ok_or(PsiError::BadDescriptor)?;
    Ok((value, &rest[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_section;

    fn sdt_section(table_id: u8) -> Vec<u8> {
        let mut service_descriptor = vec![0x48, 0x0B, 0x01, 0x04];
        service_descriptor.extend_from_slice(b"PROV");
        service_descriptor.push(0x04);
        service_descriptor.extend_from_slice(b"NAME");

        let mut payload = vec![0x00, 0x01, 0xFF]; // ONID, reserved
        payload.extend_from_slice(&0x0001u16.to_be_bytes()); // service_id
        payload.push(0x03); // EIT schedule + present/following
        payload.push(0x90); // running_status 4, free_CA_mode, length high nibble
        payload.push(service_descriptor.len() as u8);
        payload.extend_from_slice(&service_descriptor);
        build_section(table_id, 0x1234, 0, &payload)
    }

    #[test]
    fn parses_valid_actual_section() {
        let sdt = Sdt::parse(&sdt_section(SDT_ACTUAL_TABLE_ID), CrcCheck::Verify).unwrap();
        assert_eq!(sdt.header.table_id, SDT_ACTUAL_TABLE_ID);
        assert_eq!(sdt.transport_stream_id, 0x1234);
        assert_eq!(sdt.original_network_id, 0x0001);
        assert_eq!(sdt.services.len(), 1);

        let service = &sdt.services[0];
        assert_eq!(service.service_id, 1);
        assert!(service.eit_schedule);
        assert!(service.eit_present_following);
        assert_eq!(service.running_status, 4);
        assert!(service.free_ca_mode);
        assert_eq!(service.service_type, Some(1));
        assert_eq!(service.provider_name.as_deref(), Some(b"PROV".as_slice()));
        assert_eq!(service.service_name.as_deref(), Some(b"NAME".as_slice()));
        assert_eq!(service.descriptors.len(), 1);
    }

    #[test]
    fn accepts_other_transport_stream_table() {
        let sdt = Sdt::parse(&sdt_section(SDT_OTHER_TABLE_ID), CrcCheck::Verify).unwrap();
        assert_eq!(sdt.header.table_id, SDT_OTHER_TABLE_ID);
        assert_eq!(sdt.services.len(), 1);
    }

    #[test]
    fn rejects_truncated_section() {
        let mut section = sdt_section(SDT_ACTUAL_TABLE_ID);
        section.pop();
        assert_eq!(
            Sdt::parse(&section, CrcCheck::Skip),
            Err(PsiError::Truncated)
        );
    }

    #[test]
    fn rejects_bad_crc() {
        let mut section = sdt_section(SDT_ACTUAL_TABLE_ID);
        let last = section.len() - 1;
        section[last] ^= 0xFF;
        assert_eq!(
            Sdt::parse(&section, CrcCheck::Verify),
            Err(PsiError::CrcMismatch)
        );
        assert!(Sdt::parse(&section, CrcCheck::Skip).is_ok());
    }
}
