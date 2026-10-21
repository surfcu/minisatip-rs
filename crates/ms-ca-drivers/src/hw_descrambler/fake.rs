//! Recording [`CaDevice`] fake: logs every call and simulates missing or failing devices.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use super::device::{CaDescr, CaDescrData, CaDescrInfo, CaDescrMode, CaDevice, CaFd, CaPid};

/// One recorded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaCall {
    /// `open` of a path, with the fd or the error kind.
    Open(String, Result<CaFd, io::ErrorKind>),
    /// `close`.
    Close(CaFd),
    /// `CA_GET_DESCR_INFO`.
    GetDescrInfo(CaFd),
    /// `CA_SET_DESCR_MODE`.
    SetDescrMode(CaFd, CaDescrMode),
    /// `CA_SET_PID`.
    SetPid(CaFd, CaPid),
    /// `CA_SET_DESCR`.
    SetDescr(CaFd, CaDescr),
    /// `CA_SET_DESCR_DATA`.
    SetDescrData(CaFd, CaDescrData),
}

/// Fake CA device tree.
#[derive(Debug, Clone, Default)]
pub struct RecordingCaDevice {
    /// Every call in order.
    pub calls: Vec<CaCall>,
    /// Paths that open; any other path fails with the kind in `open_errors` or `NotFound`.
    pub nodes: BTreeSet<String>,
    /// Error kinds for paths that exist but fail to open.
    pub open_errors: BTreeMap<String, io::ErrorKind>,
    /// `CA_GET_DESCR_INFO` answer; `None` fails the ioctl like a driver without it.
    pub descr_info: Option<CaDescrInfo>,
    /// When set, every `CA_SET_*` ioctl fails with `EINVAL`.
    pub fail_set: bool,
    next_fd: i32,
}

impl RecordingCaDevice {
    /// No CA nodes at all, like the C++ unit-test machine.
    pub fn empty() -> Self {
        Self::default()
    }

    /// One openable node with `num` reported slots.
    pub fn with_node(path: &str, num: u32) -> Self {
        let mut dev = Self::default();
        dev.nodes.insert(path.to_string());
        dev.descr_info = Some(CaDescrInfo { num, ty: 1 });
        dev
    }

    /// Calls other than `open`/`close`/`CA_GET_DESCR_INFO`.
    pub fn set_calls(&self) -> Vec<CaCall> {
        self.calls
            .iter()
            .filter(|c| {
                !matches!(
                    c,
                    CaCall::Open(..) | CaCall::Close(_) | CaCall::GetDescrInfo(_)
                )
            })
            .cloned()
            .collect()
    }

    fn set_result(&self) -> io::Result<()> {
        if self.fail_set {
            Err(io::Error::from(io::ErrorKind::InvalidInput))
        } else {
            Ok(())
        }
    }
}

impl CaDevice for RecordingCaDevice {
    fn open(&mut self, path: &str) -> io::Result<CaFd> {
        let result = if let Some(kind) = self.open_errors.get(path) {
            Err(*kind)
        } else if self.nodes.contains(path) {
            self.next_fd += 1;
            Ok(CaFd(100 + self.next_fd))
        } else {
            Err(io::ErrorKind::NotFound)
        };
        self.calls.push(CaCall::Open(path.to_string(), result));
        result.map_err(io::Error::from)
    }

    fn close(&mut self, fd: CaFd) {
        self.calls.push(CaCall::Close(fd));
    }

    fn get_descr_info(&mut self, fd: CaFd) -> io::Result<CaDescrInfo> {
        self.calls.push(CaCall::GetDescrInfo(fd));
        self.descr_info
            .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))
    }

    fn set_descr_mode(&mut self, fd: CaFd, mode: &CaDescrMode) -> io::Result<()> {
        self.calls.push(CaCall::SetDescrMode(fd, *mode));
        self.set_result()
    }

    fn set_pid(&mut self, fd: CaFd, pid: &CaPid) -> io::Result<()> {
        self.calls.push(CaCall::SetPid(fd, *pid));
        self.set_result()
    }

    fn set_descr(&mut self, fd: CaFd, descr: &CaDescr) -> io::Result<()> {
        self.calls.push(CaCall::SetDescr(fd, *descr));
        self.set_result()
    }

    fn set_descr_data(&mut self, fd: CaFd, data: &CaDescrData) -> io::Result<()> {
        self.calls.push(CaCall::SetDescrData(fd, *data));
        self.set_result()
    }
}
