//! RTSP, HTTP, RTP/UDP/TCP, SRT and SSDP networking.
//!
//! The socket core is owned by [`socket::SocketTable`]; there is no global table.

pub mod bind;
pub mod fifo;
pub mod handler;
pub mod response;
pub mod rtsp;
pub mod satip;
pub mod session;
pub mod socket;
#[cfg(feature = "srt")]
pub mod srt;
#[cfg(feature = "srt")]
mod srt_ffi;
pub mod ssdp;
#[cfg(target_os = "linux")]
pub mod sys;
pub mod transport;
pub mod web;
pub mod writeq;

pub use handler::{
    HandlerContext, HttpRoute, PlayError, StreamBackend, StreamId, StreamInfo, StreamType,
    TransportError,
};
pub use response::{PUBLIC_HEADER, Response, format_http_date};
pub use rtsp::{
    Header, ParseOutcome, Protocol, Request, SessionHeader, TransportKind, TransportMode,
    TransportSpec, parse_request, parse_transport,
};
pub use satip::{ParseReport, merge_satip_url, parse_satip_url, parse_satip_url_new};
pub use session::{SessionManager, Stream};
