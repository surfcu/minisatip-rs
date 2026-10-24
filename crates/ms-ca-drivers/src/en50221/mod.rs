//! EN 50221 CI host stack (`ca.cpp` without CAPMT building): link, transport,
//! session layers and the application resources, one [`CaDevice`] per slot.

pub mod apps;
pub mod asn1;
pub mod capmt;
#[cfg(feature = "ci-plus")]
pub mod ciplus;
pub mod clock;
pub mod consts;
pub mod device;
pub mod link;
pub mod registry;
pub mod resources;
#[cfg(target_os = "linux")]
pub mod sys;

#[cfg(all(test, feature = "ci-plus"))]
mod ciplus_tests;
#[cfg(test)]
mod fakecam_tests;
#[cfg(test)]
mod tests;

pub use capmt::{En50221Driver, TpduLink, apdu_tpdu, asn_1_encode};
pub use clock::{Clock, ManualClock, SystemClock};
pub use device::{
    ApduDir, ApduRecord, CaDevice, CaPmtSink, CaState, HostEvent, LinkMode, ReadOutcome, Session,
};
pub use link::{AdapterInfo, AdapterKind, CaLink, CaOpener, ResetResult, SlotInfo};
pub use registry::{CaRegistry, InitResult, authdata_filename};
