//! Netceiver backend (`-n`, netceiver.cpp) over libnetceiver (libmcli).
//!
//! The state machine, tuning map and discovery run against [`McliReceiverApi`] and
//! [`McliDiscoveryApi`]; the `netceiver` feature adds the hand-written FFI in `ffi`.

mod api;
mod backend;
mod discover;
pub mod fake;
mod params;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "netceiver", target_os = "linux"))]
#[allow(unsafe_code)]
mod ffi;
#[cfg(all(target_os = "linux", any(feature = "netceiver", feature = "linux-dvb")))]
#[cfg_attr(not(feature = "netceiver"), allow(dead_code))]
mod ffi_types;

pub use api::*;
pub use backend::{MAX_PIDS, NetceiverTuner};
pub use discover::{NetcvAdapter, NetcvConfig, NetcvDiscovery, NetcvError, find_netcv_adapters};
#[cfg(all(feature = "netceiver", target_os = "linux"))]
pub use ffi::LibMcli;
pub use params::*;
