//! TS packet, PES/section assembly, CRC, PID router and zero-copy buffers.

pub mod crc;
pub mod filter;
pub mod packet;
pub mod psi;
pub mod section;
pub mod stage;

pub use crc::crc32_mpeg2;
pub use filter::{
    FILTER_CRC, FILTER_EMM, FILTER_REVERSE, Filter, FilterCallback, FilterTable, MAX_FILTERS,
};
pub use packet::{Afc, TS_PACKET_SIZE, TsPacket};
pub use psi::buffer_to_ts;
pub use section::{MAX_SECTION_SIZE, SectionAssembler, wait_pusi};
pub use stage::TsStage;
