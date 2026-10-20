//! RTSP, HTTP, RTP/UDP/TCP, SRT and SSDP networking.
//!
//! The socket core is owned by [`socket::SocketTable`]; there is no global table.

pub mod bind;
pub mod fifo;
pub mod socket;
#[cfg(target_os = "linux")]
pub mod sys;
pub mod transport;
pub mod writeq;
