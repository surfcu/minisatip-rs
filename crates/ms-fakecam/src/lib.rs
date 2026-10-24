//! EN50221/DDCI CAM simulator used by the differential harness (PLAN §6).
//!
//! [`FakeCam`] answers the host side of `ca.cpp` on the Linux CA device framing
//! (`handle_tpdu`) or raw SPDUs (`handle_spdu`), keeps the CA_PMT program list,
//! "descrambles" TS on its DDCI port and records every APDU into a [`Recorder`].

pub mod asn1;
pub mod cam;
pub mod capmt;
#[cfg(feature = "ci-plus")]
pub mod ciplus;
pub mod clock;
pub mod config;
pub mod ddci;
pub mod host;
pub mod io;
pub mod programs;
pub mod record;
pub mod tags;

pub use cam::FakeCam;
pub use capmt::{CaPmt, CaPmtReply, CaPmtStream, ListManagement};
#[cfg(feature = "ci-plus")]
pub use ciplus::{CiPlusCam, CiPlusStatus};
pub use clock::{Clock, ManualClock, MonotonicClock};
pub use config::{AppInfo, CamConfig, PmtReplyMode, Quirks};
pub use ddci::TsStats;
pub use programs::{Program, ProgramList, ProgramState};
pub use record::{Direction, Event, Record, Recorder};
