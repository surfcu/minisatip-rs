//! `Tuner` trait and backends: linux-dvb, satipc, netceiver and file-replay.

mod file;
pub mod netceiver;
pub mod satipc;
mod tuner;

#[cfg(feature = "linux-dvb")]
pub mod linux_dvb;

pub use file::{FileReplayConfig, FileReplayTuner, Pacing};
pub use tuner::{
    DeliverySystem, PID_ALL, PidSet, Polarization, Result, SignalStatus, TS_PACKET_SIZE, TsBuf,
    TuneParams, Tuner, TunerError,
};
