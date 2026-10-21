//! The libmcli (libnetceiver) calls the backend makes, and the state its callbacks share.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use super::params::McliTune;
use crate::tuner::TS_PACKET_SIZE;

/// `API_SOCK_NAMESPACE` passed to `api_sock_init`.
pub const API_SOCK_NAMESPACE: &str = "/var/tmp/mcli.sock";
/// UDP port passed to `recv_init`.
pub const MCLI_PORT: u16 = 23000;
/// `F_SETPIPE_SZ` of the C++ DVR pipe.
pub const NETCV_PIPE_SIZE: usize = 5 * 188 * 1024;
/// `FE_HAS_LOCK`.
pub const FE_HAS_LOCK: i32 = 0x10;

/// Handle of a libmcli receiver (`recv_info_t *`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiverId(pub usize);

/// `recv_festatus_t` as delivered to the TEN handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeStatus {
    /// `st` (`fe_status_t`).
    pub st: i32,
    /// `ber`.
    pub ber: u32,
    /// `strength`.
    pub strength: u16,
    /// `snr`.
    pub snr: u16,
}

/// Signal fields `handle_ten` stores on the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetcvSignal {
    /// `ad->strength`, 0..255.
    pub strength: u16,
    /// `ad->status`: `FE_HAS_LOCK` or 0.
    pub status: i32,
    /// `ad->snr`, 0..255.
    pub snr: u16,
    /// `ad->ber`.
    pub ber: u32,
}

#[derive(Debug, Default)]
struct SharedInner {
    pipe: VecDeque<u8>,
    lp: usize,
    signal: NetcvSignal,
    dropped: u64,
}

/// What libmcli's threads write into: the DVR pipe, the active PID count and the signal.
#[derive(Debug, Default)]
pub struct NetcvShared {
    inner: Mutex<SharedInner>,
}

impl NetcvShared {
    /// New shared state behind an `Arc` for the callbacks.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SharedInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `handle_ts`: queues whole TS packets while PIDs are active; always consumes `data`.
    pub fn handle_ts(&self, data: &[u8]) -> usize {
        let mut inner = self.lock();
        if inner.lp == 0 {
            return data.len();
        }
        if data.first() != Some(&0x47) || !data.len().is_multiple_of(TS_PACKET_SIZE) {
            return data.len();
        }
        let free = NETCV_PIPE_SIZE - inner.pipe.len();
        let take = data.len().min(free / TS_PACKET_SIZE * TS_PACKET_SIZE);
        inner.pipe.extend(&data[..take]);
        inner.dropped += (data.len() - take) as u64;
        data.len()
    }

    /// `handle_ten`: stores 8-bit strength/SNR and the lock bit (`st == 0x1f`).
    pub fn handle_ten(&self, status: Option<&FeStatus>) {
        let Some(s) = status else { return };
        self.lock().signal = NetcvSignal {
            strength: s.strength >> 8,
            status: if s.st == 0x1f { FE_HAS_LOCK } else { 0 },
            snr: s.snr >> 8,
            ber: s.ber,
        };
    }

    /// Latest signal.
    pub fn signal(&self) -> NetcvSignal {
        self.lock().signal
    }

    pub(crate) fn reset_signal(&self) {
        self.lock().signal = NetcvSignal::default();
    }

    pub(crate) fn set_active_pids(&self, lp: usize) {
        self.lock().lp = lp;
    }

    /// Moves up to `max` queued bytes into `out`.
    pub(crate) fn drain(&self, out: &mut Vec<u8>, max: usize) {
        let mut inner = self.lock();
        let n = inner.pipe.len().min(max);
        out.extend(inner.pipe.drain(..n));
    }

    /// Bytes `handle_ts` could not queue because the pipe was full.
    pub fn dropped_bytes(&self) -> u64 {
        self.lock().dropped
    }
}

/// Receiver-level libmcli calls (`recv_add` .. `recv_stop`).
pub trait McliReceiverApi {
    /// `recv_add` plus `register_ten_handler`/`register_ts_handler` into `shared`.
    fn recv_add(&mut self, shared: Arc<NetcvShared>) -> Option<ReceiverId>;
    /// Unregisters both handlers, then `recv_del`.
    fn recv_del(&mut self, rx: ReceiverId);
    /// `recv_tune` with an empty PID list; returns the C result.
    fn recv_tune(&mut self, rx: ReceiverId, tune: &McliTune) -> i32;
    /// `recv_pids` with `id = 0` per PID; returns the C result.
    fn recv_pids(&mut self, rx: ReceiverId, pids: &[u16]) -> i32;
    /// `recv_stop`.
    fn recv_stop(&mut self, rx: ReceiverId) -> i32;
    /// `sleep_msec`; fakes record it.
    fn sleep_ms(&mut self, ms: u64);
}

/// One tuner of a netceiver (`tuner_info_t`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetcvTunerInfo {
    /// `fe_info.name`.
    pub name: String,
    /// `fe_info.type` (`FE_QPSK` .. [`super::params::FE_DVBS2`]).
    pub fe_type: i32,
}

/// One discovered netceiver (`netceiver_info_t`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetceiverInfo {
    /// `uuid`.
    pub uuid: String,
    /// Its tuners.
    pub tuners: Vec<NetcvTunerInfo>,
}

/// Module-level libmcli calls used by `find_netcv_adapter`.
pub trait McliDiscoveryApi {
    /// Interface exists with an IPv6 address and is `IFF_UP | IFF_RUNNING | IFF_MULTICAST`.
    fn interface_ready(&mut self, ifname: &str) -> bool;
    /// `recv_init(ifname, port)`.
    fn recv_init(&mut self, ifname: &str, port: u16) -> i32;
    /// `api_sock_init(path)`.
    fn api_sock_init(&mut self, path: &str) -> i32;
    /// `nc_get_list()`.
    fn netceivers(&mut self) -> Vec<NetceiverInfo>;
    /// `nc_lock_list()`.
    fn lock_list(&mut self);
    /// `nc_unlock_list()`.
    fn unlock_list(&mut self);
    /// `sleep_msec`; fakes record it.
    fn sleep_ms(&mut self, ms: u64);
}
