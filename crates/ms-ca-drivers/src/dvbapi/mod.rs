//! DVBAPI (oscam) client: wire format, protocol state machine and transport.
//!
//! `client` is sans-IO: callers pass time in and drain [`client::DvbapiClient::take_output`].
//! `proto` builds and parses the messages; `transport` opens the TCP/unix socket.

pub mod client;
pub mod proto;
pub mod transport;

pub use client::{DvbapiClient, DvbapiConfig, Key, KeyFilter, PmtStatus, PollAction};
pub use proto::{Capmt, EcmInfo, ServerMsg, build_capmt, client_info, filter_data, parse_message};
