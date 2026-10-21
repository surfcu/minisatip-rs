//! SRT listener and output, mirroring `srt.cpp` and the SRT branch of
//! `flush_stream` (stream.cpp:795).
//!
//! The safe logic talks to the library through [`SrtOps`]; `srt_ffi`, enabled by
//! the `srt` feature, implements it over the shared `ms-srt` bindings.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// Invalid SRT socket value (`SRT_INVALID_SOCK`).
pub const SRT_INVALID_SOCK: i32 = -1;
/// Maximum sockets waiting for a matching stream (`SRT_PENDING_MAX`).
pub const SRT_PENDING_MAX: usize = 64;
/// Send chunk size used by C++ (`MAX_UDP_PACKET_SIZE`, stream.h:18).
pub const MAX_UDP_PACKET_SIZE: usize = 1316;
/// Listen backlog used by C++.
pub const SRT_LISTEN_BACKLOG: i32 = 10;

/// Failure while setting up or using the SRT listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrtError {
    /// `srt_create_socket` failed.
    CreateSocket,
    /// `srt_bind_acquire` failed.
    Bind,
    /// `srt_listen_callback` failed.
    AcceptCallback,
    /// `srt_listen` failed.
    Listen,
    /// `srt_send` failed.
    Send,
}

impl fmt::Display for SrtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSocket => write!(f, "srt_create_socket failed"),
            Self::Bind => write!(f, "srt_bind_acquire failed"),
            Self::AcceptCallback => write!(f, "srt_listen_callback failed"),
            Self::Listen => write!(f, "srt_listen failed"),
            Self::Send => write!(f, "srt_send failed"),
        }
    }
}

impl std::error::Error for SrtError {}

/// Library calls used by [`SrtListener`], allowing tests to use a fake.
pub trait SrtOps: Send + Sync {
    /// Creates a listening socket.
    fn create_socket(&self) -> i32;
    /// Binds an SRT socket to an existing UDP socket.
    fn bind_acquire(&self, sock: i32, udp_fd: i32) -> bool;
    /// Installs the pre-accept callback feeding `pending`.
    fn set_accept_callback(&self, sock: i32, pending: Arc<Mutex<PendingPool>>) -> bool;
    /// Starts listening with `backlog`.
    fn listen(&self, sock: i32, backlog: i32) -> bool;
    /// Blocking accept; returns [`SRT_INVALID_SOCK`] when the listener closes.
    fn accept(&self, sock: i32) -> i32;
    /// Closes a socket; also wakes a blocking accept when it is the listener.
    fn close(&self, sock: i32);
    /// Whether `sock` is in `SRTS_CONNECTED`.
    fn is_connected(&self, sock: i32) -> bool;
    /// Sends bytes and returns the byte count, or a negative value on failure.
    fn send(&self, sock: i32, data: &[u8]) -> i32;
}

/// Result of a pre-accept callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptDecision {
    /// The connection is accepted; `replaced` is an older socket to close.
    Accepted {
        /// Previously queued socket for the same stream id.
        replaced: i32,
    },
    /// The connection is rejected.
    Rejected,
}

/// Stream-id keyed pool of accepted sockets awaiting a SETUP.
#[derive(Debug, Default)]
pub struct PendingPool {
    sockets: HashMap<String, i32>,
}

impl PendingPool {
    /// Creates an empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies the C++ pre-accept rules (`srt_accept_poll`, srt.cpp:100).
    pub fn accept(&mut self, streamid: Option<&str>, sock: i32) -> AcceptDecision {
        let Some(streamid) = streamid.filter(|value| !value.is_empty()) else {
            // srt.cpp:104
            ms_log::log!("SRT accept: empty streamid, rejecting connection");
            return AcceptDecision::Rejected;
        };
        if self.sockets.len() >= SRT_PENDING_MAX {
            // srt.cpp:112
            ms_log::log!(
                "SRT accept: pending pool full ({}), rejecting connection",
                SRT_PENDING_MAX
            );
            return AcceptDecision::Rejected;
        }
        let mut replaced = SRT_INVALID_SOCK;
        if let Some(old) = self.sockets.get(streamid).copied()
            && old != sock
        {
            replaced = old;
            self.sockets.remove(streamid);
        }
        self.sockets.insert(streamid.to_owned(), sock);
        // srt.cpp:127
        ms_log::log!(
            "SRT accepted connection srt_sock={}, streamid='{}' ({} pending)",
            sock,
            streamid,
            self.sockets.len()
        );
        AcceptDecision::Accepted { replaced }
    }

    /// Removes and returns the socket queued for `streamid`.
    pub fn take(&mut self, streamid: &str) -> i32 {
        self.sockets.remove(streamid).unwrap_or(SRT_INVALID_SOCK)
    }

    /// Queues a connected socket for reuse; returns a replaced socket to close.
    pub fn put(&mut self, streamid: &str, sock: i32) -> i32 {
        match self.sockets.insert(streamid.to_owned(), sock) {
            Some(old) if old != sock => old,
            _ => SRT_INVALID_SOCK,
        }
    }

    /// Number of queued sockets.
    pub fn len(&self) -> usize {
        self.sockets.len()
    }

    /// Whether no socket is queued.
    pub fn is_empty(&self) -> bool {
        self.sockets.is_empty()
    }

    /// Removes every queued socket.
    pub fn drain(&mut self) -> Vec<i32> {
        self.sockets.drain().map(|(_, sock)| sock).collect()
    }
}

/// Listener plus pending pool, owning the blocking accept drain thread.
pub struct SrtListener<O: SrtOps + 'static> {
    ops: Arc<O>,
    pending: Arc<Mutex<PendingPool>>,
    listener: Arc<AtomicI32>,
    udp_fd: i32,
    drain: Option<JoinHandle<()>>,
}

impl<O: SrtOps + 'static> SrtListener<O> {
    /// Creates a listener around `ops`; call [`Self::init`] to start it.
    pub fn new(ops: Arc<O>) -> Self {
        Self {
            ops,
            pending: Arc::new(Mutex::new(PendingPool::new())),
            listener: Arc::new(AtomicI32::new(SRT_INVALID_SOCK)),
            udp_fd: SRT_INVALID_SOCK,
            drain: None,
        }
    }

    /// Shared pending pool, exposed for the pre-accept callback and tests.
    pub fn pending(&self) -> &Arc<Mutex<PendingPool>> {
        &self.pending
    }

    /// Library operations, shared with stream outputs.
    pub fn ops(&self) -> &Arc<O> {
        &self.ops
    }

    /// Whether the listener socket is live.
    pub fn is_init(&self) -> bool {
        self.listener.load(Ordering::Acquire) != SRT_INVALID_SOCK
    }

    /// Current listener socket.
    pub fn listener_socket(&self) -> i32 {
        self.listener.load(Ordering::Acquire)
    }

    /// UDP socket descriptor acquired by the listener, or -1.
    pub fn udp_fd(&self) -> i32 {
        self.udp_fd
    }

    /// Creates, binds, configures and listens, then starts the drain thread
    /// (`srt_listener_init`, srt.cpp:141). `udp_fd` must be a bound UDP socket
    /// on the RTSP port; ownership transfers to the SRT listener on success.
    pub fn init(&mut self, udp_fd: i32) -> Result<(), SrtError> {
        if self.is_init() {
            self.close();
        }
        let listener = self.ops.create_socket();
        if listener == SRT_INVALID_SOCK {
            // srt.cpp:154
            ms_log::log_and_return!(
                Err(SrtError::CreateSocket),
                "SRT listener: srt_create_socket failed: {}",
                ms_srt::last_error()
            );
        }
        if !self.ops.bind_acquire(listener, udp_fd) {
            self.ops.close(listener);
            // srt.cpp:166
            ms_log::log_and_return!(
                Err(SrtError::Bind),
                "SRT listener: srt_bind_acquire failed: {}",
                ms_srt::last_error()
            );
        }
        if !self.ops.set_accept_callback(listener, self.pending.clone()) {
            self.ops.close(listener);
            // srt.cpp:176
            ms_log::log_and_return!(
                Err(SrtError::AcceptCallback),
                "SRT listener: srt_listen_callback failed: {}",
                ms_srt::last_error()
            );
        }
        if !self.ops.listen(listener, SRT_LISTEN_BACKLOG) {
            self.ops.close(listener);
            // srt.cpp:184
            ms_log::log_and_return!(
                Err(SrtError::Listen),
                "SRT listener: srt_listen failed: {}",
                ms_srt::last_error()
            );
        }
        self.pending.lock().expect("pending pool").drain();
        if let Some(handle) = self.drain.take() {
            drop(handle);
        }
        self.udp_fd = udp_fd;
        self.listener.store(listener, Ordering::Release);

        let ops = self.ops.clone();
        let listener_slot = self.listener.clone();
        self.drain = Some(thread::spawn(move || {
            loop {
                let socket = listener_slot.load(Ordering::Acquire);
                if socket == SRT_INVALID_SOCK {
                    break;
                }
                if ops.accept(socket) == SRT_INVALID_SOCK {
                    break;
                }
            }
        }));
        Ok(())
    }

    /// Closes the listener, joins the drain and closes pending sockets
    /// (`srt_listener_close`, srt.cpp:200).
    pub fn close(&mut self) {
        let listener = self.listener.swap(SRT_INVALID_SOCK, Ordering::AcqRel);
        if listener != SRT_INVALID_SOCK {
            self.ops.close(listener);
            self.udp_fd = SRT_INVALID_SOCK;
        }
        if let Some(handle) = self.drain.take() {
            let _ = handle.join();
        }
        let dropped = self.pending.lock().expect("pending pool").drain();
        for socket in dropped {
            self.ops.close(socket);
        }
    }

    /// Takes the socket accepted with `streamid` (`srt_pending_take`).
    pub fn take(&self, streamid: &str) -> i32 {
        self.pending.lock().expect("pending pool").take(streamid)
    }

    /// Returns a socket for reuse, or closes it when disconnected
    /// (`srt_pending_return`, srt.cpp:72).
    pub fn return_socket(&self, sock: i32, streamid: &str) {
        if sock == SRT_INVALID_SOCK {
            return;
        }
        if !self.ops.is_connected(sock) {
            self.ops.close(sock);
            // srt.cpp:78
            ms_log::log!(
                "SRT socket {} closed (not connected), streamid='{}'",
                sock,
                streamid
            );
            return;
        }
        let old = self
            .pending
            .lock()
            .expect("pending pool")
            .put(streamid, sock);
        if old != SRT_INVALID_SOCK {
            self.ops.close(old);
        }
        // srt.cpp:94
        ms_log::log!(
            "SRT socket {} returned to pending pool for reuse, streamid='{}'",
            sock,
            streamid
        );
    }

    /// Sends TS data with the C++ chunking and error handling.
    pub fn send_ts(&self, sock: i32, data: &[u8]) -> Result<usize, SrtError> {
        match send_ts_partial(self.ops.as_ref(), sock, data) {
            (total, false) => Ok(total),
            _ => Err(SrtError::Send),
        }
    }

    /// Sends all iovec-like buffers in order, like C++ `flush_stream`.
    pub fn send_iov(&self, sock: i32, chunks: &[&[u8]]) -> Result<usize, SrtError> {
        let mut total = 0;
        for chunk in chunks {
            total += self.send_ts(sock, chunk)?;
        }
        Ok(total)
    }
}

/// Sends one buffer in `MAX_UDP_PACKET_SIZE` chunks and reports how many bytes
/// went out before an error, matching the SRT branch of `flush_stream`
/// (stream.cpp:800-823).
pub(crate) fn send_ts_partial(ops: &dyn SrtOps, sock: i32, data: &[u8]) -> (usize, bool) {
    let mut total = 0;
    for chunk in data.chunks(MAX_UDP_PACKET_SIZE) {
        let mut offset = 0;
        while offset < chunk.len() {
            let sent = ops.send(sock, &chunk[offset..]);
            if sent <= 0 {
                return (total, true);
            }
            let sent = sent as usize;
            total += sent;
            offset += sent;
        }
    }
    (total, false)
}

impl<O: SrtOps + 'static> Drop for SrtListener<O> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(feature = "srt")]
pub use crate::srt_ffi::LibSrtOps;
