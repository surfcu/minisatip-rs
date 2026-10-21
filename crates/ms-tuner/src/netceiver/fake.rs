//! Recording libmcli fake for tests.

use std::sync::Arc;

use super::api::{McliDiscoveryApi, McliReceiverApi, NetceiverInfo, NetcvShared, ReceiverId};
use super::params::McliTune;

/// One recorded libmcli call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McliCall {
    /// `recv_add` (+ handler registration), with the handle it returned.
    RecvAdd(Option<ReceiverId>),
    /// Handler unregistration + `recv_del`.
    RecvDel(ReceiverId),
    /// `recv_tune`.
    RecvTune(ReceiverId, McliTune),
    /// `recv_pids`.
    RecvPids(ReceiverId, Vec<u16>),
    /// `recv_stop`.
    RecvStop(ReceiverId),
    /// `sleep_msec`.
    Sleep(u64),
    /// Interface check.
    InterfaceReady(String),
    /// `recv_init`.
    RecvInit(String, u16),
    /// `api_sock_init`.
    ApiSockInit(String),
    /// `nc_get_list`.
    GetList,
    /// `nc_lock_list`.
    LockList,
    /// `nc_unlock_list`.
    UnlockList,
}

/// Fake libmcli: records calls; `handlers` keeps the registered callback state.
#[derive(Debug, Default)]
pub struct FakeMcli {
    /// Calls in order.
    pub calls: Vec<McliCall>,
    /// When set, `recv_add` returns NULL.
    pub fail_add: bool,
    /// Callback state of live receivers, by handle.
    pub handlers: Vec<(ReceiverId, Arc<NetcvShared>)>,
    /// Interface check answer.
    pub interface_up: bool,
    /// Successive `nc_get_list` answers; the last one repeats.
    pub lists: Vec<Vec<NetceiverInfo>>,
    next: usize,
    list_calls: usize,
}

impl FakeMcli {
    /// Fake with the interface up and no netceivers.
    pub fn new() -> Self {
        Self {
            interface_up: true,
            ..Self::default()
        }
    }

    /// Calls other than sleeps.
    pub fn receiver_calls(&self) -> Vec<McliCall> {
        self.calls
            .iter()
            .filter(|c| !matches!(c, McliCall::Sleep(_)))
            .cloned()
            .collect()
    }

    /// Delivers TS data the way libmcli's receive thread would.
    pub fn deliver_ts(&self, rx: ReceiverId, data: &[u8]) -> usize {
        self.shared(rx).map_or(0, |s| s.handle_ts(data))
    }

    /// The callback state registered for `rx`.
    pub fn shared(&self, rx: ReceiverId) -> Option<&Arc<NetcvShared>> {
        self.handlers
            .iter()
            .find(|(id, _)| *id == rx)
            .map(|(_, s)| s)
    }
}

impl McliReceiverApi for FakeMcli {
    fn recv_add(&mut self, shared: Arc<NetcvShared>) -> Option<ReceiverId> {
        let rx = if self.fail_add {
            None
        } else {
            self.next += 1;
            let rx = ReceiverId(self.next);
            self.handlers.push((rx, shared));
            Some(rx)
        };
        self.calls.push(McliCall::RecvAdd(rx));
        rx
    }

    fn recv_del(&mut self, rx: ReceiverId) {
        self.handlers.retain(|(id, _)| *id != rx);
        self.calls.push(McliCall::RecvDel(rx));
    }

    fn recv_tune(&mut self, rx: ReceiverId, tune: &McliTune) -> i32 {
        self.calls.push(McliCall::RecvTune(rx, *tune));
        0
    }

    fn recv_pids(&mut self, rx: ReceiverId, pids: &[u16]) -> i32 {
        self.calls.push(McliCall::RecvPids(rx, pids.to_vec()));
        0
    }

    fn recv_stop(&mut self, rx: ReceiverId) -> i32 {
        self.calls.push(McliCall::RecvStop(rx));
        0
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.calls.push(McliCall::Sleep(ms));
    }
}

impl McliDiscoveryApi for FakeMcli {
    fn interface_ready(&mut self, ifname: &str) -> bool {
        self.calls.push(McliCall::InterfaceReady(ifname.into()));
        self.interface_up
    }

    fn recv_init(&mut self, ifname: &str, port: u16) -> i32 {
        self.calls.push(McliCall::RecvInit(ifname.into(), port));
        0
    }

    fn api_sock_init(&mut self, path: &str) -> i32 {
        self.calls.push(McliCall::ApiSockInit(path.into()));
        0
    }

    fn netceivers(&mut self) -> Vec<NetceiverInfo> {
        self.calls.push(McliCall::GetList);
        let i = self.list_calls.min(self.lists.len().saturating_sub(1));
        self.list_calls += 1;
        self.lists.get(i).cloned().unwrap_or_default()
    }

    fn lock_list(&mut self) {
        self.calls.push(McliCall::LockList);
    }

    fn unlock_list(&mut self) {
        self.calls.push(McliCall::UnlockList);
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.calls.push(McliCall::Sleep(ms));
    }
}
