//! SAT>IP client backend (`satipc.cpp`): tunes a remote SAT>IP server over RTSP
//! and receives its TS over RTP/UDP or RTP interleaved on the RTSP connection.

mod client;
mod config;
mod driver;
mod media;
mod sys;
mod util;

pub use client::{
    Client, LastCmd, MAX_PIDS, MAX_QUALITY, MAX_STRENGTH, Op, PidSlot, PidState, ReplyOutcome,
    State, Transport,
};
pub use config::{
    MAX_DVBAPI_SYSTEMS, SatipServer, SatipXml, fetch_satip_xml, parse_satip_servers,
    parse_satip_xml,
};
pub use driver::{Driver, SatipcConfig, SatipcTuner};
pub use media::{
    Frame, MAX_RTP_MSG, MAX_UDP_PACKET_SIZE, RTP_HEADER, TcpDemux, process_rtp_datagram,
};
pub use util::{delsys_code, delsys_lookup, delsys_name, is_rtsp_response};
