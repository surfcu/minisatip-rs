//! PAT/PMT/CAT/SDT parsing, versioning and DVB charset tables.

mod cat;
mod charset;
mod charset_tables;
mod descriptor;
mod error;
mod header;
mod pat;
mod pmt;
mod sdt;
mod version;

pub use cat::{CAT_TABLE_ID, Cat};
pub use charset::{CharsetOverride, dvb_get_string};
pub use descriptor::{
    Descriptor, DescriptorIter, iter_descriptors, parse_descriptor, parse_descriptors,
};
pub use error::PsiError;
pub use header::{CrcCheck, SectionHeader, parse_header};
pub use pat::{PAT_TABLE_ID, Pat, PatEntry};
pub use pmt::{PMT_TABLE_ID, Pmt, PmtStream, is_audio_stream_type, is_video_stream_type};
pub use sdt::{SDT_ACTUAL_TABLE_ID, SDT_OTHER_TABLE_ID, Sdt, SdtService};
pub use version::{VersionStatus, VersionTracker};

#[cfg(test)]
pub(crate) mod test_util {
    use ms_ts::crc32_mpeg2;

    pub(crate) fn build_section(
        table_id: u8,
        table_id_extension: u16,
        version: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let section_length = 5 + payload.len() + 4;
        assert!(section_length <= 0x0FFF);
        let mut section = Vec::with_capacity(3 + section_length);
        section.push(table_id);
        section.push(0xB0 | (((section_length >> 8) as u8) & 0x0F));
        section.push((section_length & 0xFF) as u8);
        section.extend_from_slice(&table_id_extension.to_be_bytes());
        section.push(0xC1 | ((version & 0x1F) << 1));
        section.push(0);
        section.push(0);
        section.extend_from_slice(payload);
        let crc = crc32_mpeg2(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        section
    }
}
