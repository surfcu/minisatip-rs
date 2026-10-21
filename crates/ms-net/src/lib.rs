//! RTSP, HTTP, RTP/UDP/TCP, SRT and SSDP networking.
//!
//! The socket core is owned by [`socket::SocketTable`]; there is no global table.

pub mod bind;
pub mod fifo;
pub mod handler;
pub mod response;
pub mod rtsp;
pub mod satip;
pub mod socket;
#[cfg(target_os = "linux")]
pub mod sys;
pub mod transport;
pub mod writeq;

pub use handler::{
    HandlerContext, PlayError, StreamBackend, StreamId, StreamInfo, StreamType, TransportError,
};
pub use response::{PUBLIC_HEADER, Response, format_http_date};
pub use rtsp::{
    Header, ParseOutcome, Protocol, Request, SessionHeader, TransportKind, TransportMode,
    TransportSpec, parse_request, parse_transport,
};
pub use satip::{ParseReport, parse_satip_url, parse_satip_url_new};
