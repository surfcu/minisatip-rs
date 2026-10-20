//! Owning socket table with read dispatch, write queues and timeouts.

use crate::writeq::WriteQueue;
use std::io::{self, Write};

/// Maximum number of registered sockets, matching `MAX_SOCKS`.
pub const MAX_SOCKS: usize = 1024;
/// Read buffer size used by `select_and_execute` (`sizeof(buf) - 1`).
pub const READ_BUFFER_LEN: usize = 10240 - 1;
/// FIFO size used when `--max-sbuf` is at its default of 10 MiB.
pub const DEFAULT_FIFO_SIZE: usize = 10 * 1024 * 1024;

/// Index of a socket in a [`SocketTable`].
pub type SocketId = usize;

/// Socket purpose, mirroring the C++ `TYPE_*` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    /// UDP datagram socket.
    Udp,
    /// TCP client socket.
    Tcp,
    /// TCP listening socket.
    Server,
    /// HTTP connection.
    Http,
    /// RTSP connection.
    Rtsp,
    /// DVR handle.
    Dvr,
    /// RTCP socket.
    Rtcp,
    /// RTP socket.
    Rtp,
}

impl SocketType {
    /// Whether this is a listening socket (actions are not dispatched).
    pub fn is_server(self) -> bool {
        matches!(self, Self::Server)
    }
}

/// Read hook: fills the buffer and reports bytes read or an I/O error.
pub type ReadAction = Box<dyn FnMut(&mut [u8]) -> io::Result<usize> + Send>;
/// Action run after readable data has been buffered.
pub type Action = Box<dyn FnMut(&[u8]) -> i32 + Send>;
/// Cleanup hook run when a socket is removed.
pub type CloseAction = Box<dyn FnMut() + Send>;
/// Idle hook; a non-zero return removes the socket.
pub type TimeoutAction = Box<dyn FnMut() -> i32 + Send>;

/// Result of dispatching one readable event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// No such enabled socket.
    Missing,
    /// Nothing to do.
    Idle,
    /// Data is buffered but no action ran.
    Buffered,
    /// The action ran.
    Dispatched,
    /// The peer closed or the read failed; the socket was removed.
    Closed,
}

enum ReadStep {
    Data(usize),
    WouldBlock,
    Interrupted,
    Eof,
    Error,
    Idle,
}

/// One socket and its output queue.
pub struct Socket {
    id: SocketId,
    enabled: bool,
    handle: i32,
    kind: SocketType,
    sid: i32,
    master: Option<SocketId>,
    queue: WriteQueue,
    read_buf: Vec<u8>,
    read_len: usize,
    read: Option<ReadAction>,
    action: Option<Action>,
    close: Option<CloseAction>,
    timeout: Option<TimeoutAction>,
    rtime_ms: i64,
    timeout_ms: i64,
    force_close: bool,
}

impl Socket {
    fn new(id: SocketId, handle: i32, kind: SocketType, sid: i32, fifo_size: usize) -> Self {
        Self {
            id,
            enabled: true,
            handle,
            kind,
            sid,
            master: None,
            queue: WriteQueue::new(fifo_size),
            read_buf: vec![0; READ_BUFFER_LEN],
            read_len: 0,
            read: None,
            action: None,
            close: None,
            timeout: None,
            rtime_ms: 0,
            timeout_ms: 0,
            force_close: false,
        }
    }

    /// Table index of this socket.
    pub fn id(&self) -> SocketId {
        self.id
    }

    /// Underlying file descriptor or handle, as passed to `add`.
    pub fn handle(&self) -> i32 {
        self.handle
    }

    /// Socket purpose.
    pub fn kind(&self) -> SocketType {
        self.kind
    }

    /// Stream or adapter id attached to the socket.
    pub fn sid(&self) -> i32 {
        self.sid
    }

    /// Whether the socket is still registered.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Bytes currently buffered by the last read.
    pub fn read_len(&self) -> usize {
        self.read_len
    }

    /// Buffered read data.
    pub fn read_buffer(&self) -> &[u8] {
        &self.read_buf[..self.read_len]
    }

    /// The per-socket write queue.
    pub fn queue(&self) -> &WriteQueue {
        &self.queue
    }

    /// Mutable access to the write queue.
    pub fn queue_mut(&mut self) -> &mut WriteQueue {
        &mut self.queue
    }

    /// Installs the read hook.
    pub fn set_read<F>(&mut self, read: F)
    where
        F: FnMut(&mut [u8]) -> io::Result<usize> + Send + 'static,
    {
        self.read = Some(Box::new(read));
    }

    /// Installs the action run after readable data arrives.
    pub fn set_action<F>(&mut self, action: F)
    where
        F: FnMut(&[u8]) -> i32 + Send + 'static,
    {
        self.action = Some(Box::new(action));
    }

    /// Installs the removal hook.
    pub fn set_close<F>(&mut self, close: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.close = Some(Box::new(close));
    }

    /// Installs the idle hook.
    pub fn set_timeout<F>(&mut self, timeout: F)
    where
        F: FnMut() -> i32 + Send + 'static,
    {
        self.timeout = Some(Box::new(timeout));
    }

    /// Sets the idle timeout in milliseconds; zero disables it.
    pub fn set_timeout_ms(&mut self, timeout_ms: i64) {
        self.timeout_ms = timeout_ms;
    }

    /// Last read time in milliseconds.
    pub fn rtime_ms(&self) -> i64 {
        self.rtime_ms
    }

    /// Sets the last read time.
    pub fn set_rtime_ms(&mut self, rtime_ms: i64) {
        self.rtime_ms = rtime_ms;
    }

    /// Routes this socket's reads through `master`'s buffer and action.
    pub fn set_master(&mut self, master: Option<SocketId>) {
        self.master = master;
    }

    /// Marks the socket for closing after queued data drains.
    pub fn set_force_close(&mut self, force_close: bool) {
        self.force_close = force_close;
        self.queue.set_force_close(force_close);
    }

    /// Requests that queued data be dropped up to the next RTSP frame.
    pub fn set_flush_enqueued_data(&mut self, flush: bool) {
        self.queue.set_flush_enqueued_data(flush);
    }

    /// Writes data through the queue without priority.
    pub fn write_vectored<W: Write + ?Sized>(&mut self, sink: &mut W, chunks: &[&[u8]]) -> i32 {
        self.queue.write_vectored(sink, chunks, false)
    }

    /// Writes a buffer through the queue without priority.
    pub fn write<W: Write + ?Sized>(&mut self, sink: &mut W, data: &[u8]) -> i32 {
        self.queue.write(sink, data, false)
    }

    /// Flushes queued data and arms the close timeout once it is empty.
    pub fn flush<W: Write + ?Sized>(&mut self, sink: &mut W) -> i32 {
        let written = self.queue.flush(sink);
        if self.force_close && self.queue.fifo().used() == 0 {
            self.timeout_ms = 1;
        }
        written
    }

    fn apply_step(&mut self, step: ReadStep, now_ms: i64) -> ReadOutcome {
        match step {
            ReadStep::Data(count) => {
                self.read_len += count;
                self.rtime_ms = now_ms;
                if self.dispatch() {
                    ReadOutcome::Dispatched
                } else {
                    ReadOutcome::Buffered
                }
            }
            ReadStep::WouldBlock => {
                if self.dispatch() {
                    ReadOutcome::Dispatched
                } else {
                    ReadOutcome::Idle
                }
            }
            ReadStep::Interrupted if self.read_len > 0 => {
                if self.dispatch() {
                    ReadOutcome::Dispatched
                } else {
                    ReadOutcome::Buffered
                }
            }
            ReadStep::Interrupted | ReadStep::Idle => ReadOutcome::Idle,
            ReadStep::Eof | ReadStep::Error => ReadOutcome::Closed,
        }
    }

    fn dispatch(&mut self) -> bool {
        if self.kind.is_server() {
            return false;
        }
        let mut action = self.action.take();
        let called = match action.as_mut() {
            Some(action) => {
                action(&self.read_buf[..self.read_len]);
                true
            }
            None => false,
        };
        self.action = action;
        called
    }
}

/// Registry of sockets owned by one event loop; there is no global table.
pub struct SocketTable {
    sockets: Vec<Option<Socket>>,
    fifo_size: usize,
}

impl Default for SocketTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SocketTable {
    /// Creates an empty table with the default FIFO size.
    pub fn new() -> Self {
        Self::with_fifo_size(DEFAULT_FIFO_SIZE)
    }

    /// Creates an empty table with a custom per-socket FIFO size.
    pub fn with_fifo_size(fifo_size: usize) -> Self {
        Self {
            sockets: Vec::new(),
            fifo_size,
        }
    }

    /// Registers a socket and returns its id.
    pub fn add(&mut self, kind: SocketType, sid: i32) -> Option<SocketId> {
        self.add_handle(-1, kind, sid)
    }

    /// Registers a socket with an OS handle and returns its id.
    pub fn add_handle(&mut self, handle: i32, kind: SocketType, sid: i32) -> Option<SocketId> {
        if let Some(id) = self.sockets.iter().position(Option::is_none) {
            self.sockets[id] = Some(Socket::new(id, handle, kind, sid, self.fifo_size));
            return Some(id);
        }
        if self.sockets.len() >= MAX_SOCKS {
            return None;
        }
        let id = self.sockets.len();
        self.sockets
            .push(Some(Socket::new(id, handle, kind, sid, self.fifo_size)));
        Some(id)
    }

    /// Looks up an enabled socket.
    pub fn get(&self, id: SocketId) -> Option<&Socket> {
        self.sockets
            .get(id)
            .and_then(Option::as_ref)
            .filter(|socket| socket.enabled)
    }

    /// Mutable lookup of an enabled socket.
    pub fn get_mut(&mut self, id: SocketId) -> Option<&mut Socket> {
        self.sockets
            .get_mut(id)
            .and_then(Option::as_mut)
            .filter(|socket| socket.enabled)
    }

    /// Number of enabled sockets.
    pub fn len(&self) -> usize {
        self.sockets
            .iter()
            .filter(|slot| slot.as_ref().is_some_and(|socket| socket.enabled))
            .count()
    }

    /// Whether no sockets are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterates over enabled sockets.
    pub fn iter(&self) -> impl Iterator<Item = &Socket> {
        self.sockets
            .iter()
            .filter_map(Option::as_ref)
            .filter(|socket| socket.enabled)
    }

    /// Removes a socket, its slaves, and runs its close hook.
    pub fn remove(&mut self, id: SocketId) -> bool {
        let Some(mut socket) = self.sockets.get_mut(id).and_then(Option::take) else {
            return false;
        };
        if !socket.enabled {
            self.sockets[id] = Some(socket);
            return false;
        }
        socket.enabled = false;
        if let Some(mut close) = socket.close.take() {
            close();
        }
        let slaves: Vec<SocketId> = self
            .sockets
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.as_ref()
                    .filter(|slave| slave.enabled && slave.master == Some(id))
                    .map(|_| index)
            })
            .collect();
        for slave in slaves {
            self.remove(slave);
        }
        true
    }

    /// Reads once from `id` and dispatches its action, mirroring `select_and_execute`.
    pub fn on_readable(&mut self, id: SocketId, now_ms: i64) -> ReadOutcome {
        let Some(mut slave) = self.sockets.get_mut(id).and_then(Option::take) else {
            return ReadOutcome::Missing;
        };
        if !slave.enabled {
            self.sockets[id] = Some(slave);
            return ReadOutcome::Missing;
        }

        let mut master = None;
        let mut master_id = None;
        if let Some(candidate) = slave.master.filter(|master| *master != id)
            && let Some(socket) = self.sockets.get_mut(candidate).and_then(Option::take)
        {
            if socket.enabled {
                master = Some(socket);
                master_id = Some(candidate);
            } else {
                self.sockets[candidate] = Some(socket);
            }
        }

        let outcome = match master.as_mut() {
            Some(target) => {
                let step =
                    perform_read(&mut slave.read, &mut target.read_buf, &mut target.read_len);
                target.apply_step(step, now_ms)
            }
            None => {
                let step = perform_read(&mut slave.read, &mut slave.read_buf, &mut slave.read_len);
                slave.apply_step(step, now_ms)
            }
        };

        if let (Some(target), Some(index)) = (master, master_id) {
            self.sockets[index] = Some(target);
        }
        self.sockets[id] = Some(slave);
        if outcome == ReadOutcome::Closed {
            self.remove(id);
        }
        outcome
    }

    /// Removes idle or forced-close sockets and returns their ids.
    pub fn poll_timeouts(&mut self, now_ms: i64) -> Vec<SocketId> {
        let due: Vec<SocketId> = self
            .sockets
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| {
                let socket = slot.as_ref()?;
                if !socket.enabled {
                    return None;
                }
                let idle = socket.timeout_ms > 0
                    && now_ms - socket.rtime_ms > socket.timeout_ms
                    && socket.queue.fifo().used() == 0;
                (idle || socket.force_close).then_some(id)
            })
            .collect();

        let mut removed = Vec::new();
        for id in due {
            let run_timeout = self
                .sockets
                .get(id)
                .and_then(Option::as_ref)
                .is_some_and(|socket| !socket.force_close && socket.timeout.is_some());
            if run_timeout {
                let mut timeout = self
                    .sockets
                    .get_mut(id)
                    .and_then(Option::as_mut)
                    .and_then(|socket| socket.timeout.take());
                let result = timeout.as_mut().map(|hook| hook()).unwrap_or(0);
                if let Some(socket) = self.sockets.get_mut(id).and_then(Option::as_mut) {
                    socket.timeout = timeout;
                }
                if result != 0 && self.remove(id) {
                    removed.push(id);
                }
            } else if self.remove(id) {
                removed.push(id);
            }
        }
        removed
    }
}

fn perform_read(
    read: &mut Option<ReadAction>,
    buffer: &mut [u8],
    read_len: &mut usize,
) -> ReadStep {
    if *read_len >= buffer.len() {
        *read_len = 0;
    }
    let Some(read) = read.as_mut() else {
        return ReadStep::Idle;
    };
    match read(&mut buffer[*read_len..]) {
        Ok(0) => ReadStep::Eof,
        Ok(count) => ReadStep::Data(count),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => ReadStep::WouldBlock,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => ReadStep::Interrupted,
        Err(_) => ReadStep::Error,
    }
}
