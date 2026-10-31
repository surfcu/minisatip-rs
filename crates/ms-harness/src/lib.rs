//! Differential test runner that compares the C++ and Rust binaries (PLAN §5).
//!
//! Both binaries get the same generated TS capture, arguments and YAML
//! scenario; their normalised transcripts must be identical.

pub mod analysis;
pub mod client;
pub mod diff;
#[cfg(unix)]
pub mod fakecam;
pub mod fakeoscam;
pub mod normalize;
pub mod runner;
pub mod scenario;
pub mod server;
pub mod tsgen;

pub use runner::{Comparison, Outcome, RunOptions, compare, run_once};
pub use scenario::Scenario;
pub use server::Binary;
