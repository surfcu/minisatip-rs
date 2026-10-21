//! Linux DVB backend: frontend tuning, demux/dvr filters, DiSEqC, unicable/JESS.
//!
//! The device-facing code lives in [`device`]; the hardware syscall layer is
//! isolated in [`sys`] so the backend logic can be exercised with a fake.

mod consts;
mod device;
mod diseqc;
mod enumerate;
mod ioc;
mod signal;
mod state;
mod transponder;

#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys;

pub use consts::*;
pub use device::*;
pub use diseqc::*;
pub use enumerate::*;
pub use ioc::*;
pub use signal::*;
pub use state::*;
pub use transponder::*;

#[cfg(target_os = "linux")]
pub use backend::LinuxDvbTuner;
#[cfg(target_os = "linux")]
pub use sys::{LinuxDeviceTree, LinuxFrontendProbe};
