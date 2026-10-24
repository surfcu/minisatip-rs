//! Per-socket write queue with a high-priority lane, ported from socketworks.

use crate::fifo::{Fifo, copy_iovec_to_fifo, find_next_rtsp_header};
use std::io::{self, IoSlice, Write};

/// Cap on queued high-priority bytes (`MAX_PRIO_DATA`).
pub const MAX_PRIO_DATA: usize = 1024 * 1024;

/// Buffered output for one socket: FIFO data plus high-priority replies.
#[derive(Debug)]
pub struct WriteQueue {
    fifo: Fifo,
    prio_data: Vec<u8>,
    flush_enqueued_data: bool,
    overflow: u64,
    buffered_bytes: u64,
    force_close: bool,
    max_prio_data: usize,
    // C++ `s->id`, `s->sock` and `s->sid`, used by the write-path logs.
    log_id: i32,
    log_handle: i32,
    log_sid: i32,
}

impl WriteQueue {
    /// Creates a queue whose FIFO holds `fifo_size` bytes.
    pub fn new(fifo_size: usize) -> Self {
        Self {
            fifo: Fifo::new(fifo_size),
            prio_data: Vec::new(),
            flush_enqueued_data: false,
            overflow: 0,
            buffered_bytes: 0,
            force_close: false,
            max_prio_data: MAX_PRIO_DATA,
            log_id: -1,
            log_handle: -1,
            log_sid: -1,
        }
    }

    /// Records the C++ socket identity used by the write-path logs.
    pub fn set_log_ids(&mut self, id: i32, handle: i32, sid: i32) {
        self.log_id = id;
        self.log_handle = handle;
        self.log_sid = sid;
    }

    /// The C++ `(s->id, s->sock, s->sid)` triple, copied for log calls.
    fn log_ids(&self) -> (i32, i32, i32) {
        (self.log_id, self.log_handle, self.log_sid)
    }

    /// Creates a queue with a custom high-priority cap, for tests.
    pub fn with_prio_limit(fifo_size: usize, max_prio_data: usize) -> Self {
        let mut queue = Self::new(fifo_size);
        queue.max_prio_data = max_prio_data;
        queue
    }

    /// The underlying FIFO.
    pub fn fifo(&self) -> &Fifo {
        &self.fifo
    }

    /// Mutable access to the FIFO, used by tests to seed indices.
    pub fn fifo_mut(&mut self) -> &mut Fifo {
        &mut self.fifo
    }

    /// Queued high-priority bytes.
    pub fn prio_data(&self) -> &[u8] {
        &self.prio_data
    }

    /// Number of queued high-priority bytes.
    pub fn prio_len(&self) -> usize {
        self.prio_data.len()
    }

    /// Bytes dropped because the FIFO had no space.
    pub fn overflow(&self) -> u64 {
        self.overflow
    }

    /// Total bytes copied into the FIFO over the queue's lifetime.
    pub fn buffered_bytes(&self) -> u64 {
        self.buffered_bytes
    }

    /// Requests that queued data be dropped up to the next RTSP frame.
    pub fn set_flush_enqueued_data(&mut self, flush: bool) {
        self.flush_enqueued_data = flush;
    }

    /// Marks the socket for closing once queued data is flushed.
    pub fn set_force_close(&mut self, force_close: bool) {
        self.force_close = force_close;
    }

    /// Appends high-priority data, rejecting the whole chunk when capped.
    pub fn enqueue_high_prio(&mut self, chunks: &[&[u8]]) -> i32 {
        let len: usize = chunks.iter().map(|chunk| chunk.len()).sum();
        let pos = self.prio_data.len();
        if len > self.max_prio_data.saturating_sub(pos) {
            // socketworks.cpp:1368-1370.
            ms_log::log!(
                "sock {}: dropping {} bytes of high priority data, {} queued",
                self.log_id,
                len,
                pos
            );
            return -1;
        }
        for chunk in chunks {
            self.prio_data.extend_from_slice(chunk);
        }
        0
    }

    /// Writes data directly or buffers it, optionally on the priority lane.
    pub fn write_vectored<W: Write + ?Sized>(
        &mut self,
        sink: &mut W,
        chunks: &[&[u8]],
        high_prio: bool,
    ) -> i32 {
        let len: usize = chunks.iter().map(|chunk| chunk.len()).sum();
        if len == 0 {
            return 0;
        }

        let mut offset = 0;
        // C++ `rv`: `-EWOULDBLOCK` is converted to 0 by sockets_writev_prio.
        let mut rv = 0i32;
        let (log_id, log_handle, log_sid) = self.log_ids();
        if self.fifo.used() == 0 {
            match write_chunks(sink, chunks, log_id, log_handle, log_sid) {
                Ok(None) => {}
                Ok(Some(written)) => {
                    rv = written as i32;
                    if written == 0 || written == len {
                        self.flush_enqueued_data = false;
                        return written as i32;
                    }
                    offset = written;
                }
                Err(_) => {
                    self.flush_enqueued_data = false;
                    return -1;
                }
            }
        }

        if high_prio && !self.flush_enqueued_data && self.fifo.used() > 0 {
            return self.enqueue_high_prio(chunks);
        }

        self.flush_enqueued_if_needed();
        let copied = copy_iovec_to_fifo(&mut self.fifo, offset, chunks);
        if copied == 0 {
            // socketworks.cpp:1489-1496: the message is emitted when no
            // overflow has happened yet or LOG_SOCKET is enabled.
            if self.overflow == 0 || ms_log::enabled(ms_log::LOG_SOCKET) {
                ms_log::log!(
                    "Insufficient bandwidth: sock {}: overflow {} bytes, total {:.1} MB, len {}, offset {}, rv {}, available {}",
                    log_id,
                    len - offset,
                    self.overflow as f64 / 1_048_576.0,
                    len,
                    offset,
                    rv,
                    self.fifo.available()
                );
            }
            self.overflow = self.overflow.saturating_add((len - offset) as u64);
            return 0;
        }
        if copied + offset != len {
            // socketworks.cpp:1500-1502.
            ms_log::log!(
                "Unhandled sock {} data: copied {} offset {} len {}",
                log_id,
                copied,
                offset,
                len
            );
        }
        // socketworks.cpp:1503-1504 (LOGM under LOG_SOCKET).
        ms_log::logl!(
            ms_log::LOG_SOCKET,
            "got {} bytes, offset {}, rv {}, buffered {} bytes - used {}",
            len,
            offset,
            rv,
            copied,
            self.fifo.used()
        );
        self.buffered_bytes = self.buffered_bytes.saturating_add(copied as u64);
        offset as i32
    }

    /// Convenience wrapper for a single buffer.
    pub fn write<W: Write + ?Sized>(&mut self, sink: &mut W, data: &[u8], high_prio: bool) -> i32 {
        self.write_vectored(sink, &[data], high_prio)
    }

    /// Flushes priority data first when present, then as much FIFO data as possible.
    pub fn flush<W: Write + ?Sized>(&mut self, sink: &mut W) -> i32 {
        self.flush_enqueued_if_needed();
        if self.force_close || !self.prio_data.is_empty() {
            return self.flush_prio(sink);
        }
        let used = self.fifo.used() as usize;
        self.flush_all(sink, used)
    }

    fn flush_enqueued_if_needed(&mut self) {
        if !self.flush_enqueued_data {
            return;
        }
        if let Some(offset) = find_next_rtsp_header(&self.fifo) {
            let read_index = self.fifo.read_index();
            // socketworks.cpp:1438-1443.
            ms_log::log!(
                "sock {} dropping {} enqueued bytes (offset {})",
                self.log_id,
                self.fifo.write_index() - (read_index + offset),
                offset
            );
            self.fifo.set_indices(read_index, read_index + offset);
        }
        self.flush_enqueued_data = false;
    }

    fn flush_all<W: Write + ?Sized>(&mut self, sink: &mut W, used: usize) -> i32 {
        if used == 0 {
            return 0;
        }
        let (log_id, log_handle, log_sid) = self.log_ids();
        let start = ms_log::enabled(ms_log::LOG_SOCKET).then(std::time::Instant::now);
        let result = {
            let (first, second) = self.fifo.peek(used, 0);
            let chunks: [&[u8]; 2] = [first, second];
            let count = if second.is_empty() { 1 } else { 2 };
            write_chunks(sink, &chunks[..count], log_id, log_handle, log_sid)
        };
        match result {
            Ok(None) => 0,
            Ok(Some(written)) if written > 0 => {
                self.fifo.skip(written as u64);
                let elapsed = start
                    .map(|start| i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX))
                    .unwrap_or(0);
                // socketworks.cpp:1539-1541 (LOGM under LOG_SOCKET).
                ms_log::logl!(
                    ms_log::LOG_SOCKET,
                    "SOCK {}: flushed {} (out of {}) bytes in {} ms",
                    log_id,
                    written,
                    used,
                    elapsed
                );
                written as i32
            }
            Ok(Some(_)) => 0,
            Err(_) => -1,
        }
    }

    fn flush_prio<W: Write + ?Sized>(&mut self, sink: &mut W) -> i32 {
        let start = std::time::Instant::now();
        let (log_id, log_handle, log_sid) = self.log_ids();
        let mut rv = 0i32;
        let mut left = find_next_rtsp_header(&self.fifo)
            .map(|offset| offset as i64)
            .unwrap_or(-1);
        if left > 0 {
            let written = self.flush_all(sink, left as usize);
            rv = written;
            if written >= 0 {
                left -= written as i64;
            }
        }
        if left > 0 {
            let elapsed = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
            // socketworks.cpp:1557-1561 (LOG_AND_RETURN).
            ms_log::log_and_return!(
                left as i32,
                "sock {} flushed incomplete packet before priority response: left {}, rv {} in {} ms",
                log_id,
                left,
                rv,
                elapsed
            );
        }

        let prio_len = self.prio_data.len();
        let result = write_chunks(sink, &[&self.prio_data], log_id, log_handle, log_sid);
        let written_rv = match &result {
            Ok(Some(written)) => *written as i32,
            Ok(None) => -libc::EWOULDBLOCK,
            Err(_) => -1,
        };
        // socketworks.cpp:1566-1567.
        ms_log::log!(
            "sock {}, flushed {} bytes of priority data from {}",
            log_id,
            written_rv,
            prio_len
        );
        match result {
            Ok(None) | Ok(Some(0)) | Err(_) => 1,
            Ok(Some(written)) if written != prio_len => {
                let elapsed = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
                // socketworks.cpp:1570-1576.
                ms_log::log!(
                    "sock {} incomplete priority data write {} left {} in {} ms",
                    log_id,
                    written,
                    prio_len - written,
                    elapsed
                );
                self.prio_data.drain(..written);
                1
            }
            Ok(Some(written)) => {
                self.prio_data.clear();
                written as i32
            }
        }
    }
}

/// `my_writev` (socketworks.cpp:1294-1356): writes the chunks and emits the
/// write-path logs. `log_id`/`log_handle` were `s->id`/`s->sock`, `log_sid`
/// was `s->sid`; the Rust queue has no peer sockaddr, so the "remote" fields
/// of the refused-connection line use an empty host and port 0.
fn write_chunks<W: Write + ?Sized>(
    sink: &mut W,
    chunks: &[&[u8]],
    log_id: i32,
    log_handle: i32,
    log_sid: i32,
) -> io::Result<Option<usize>> {
    let len: usize = chunks.iter().map(|chunk| chunk.len()).sum();
    // socketworks.cpp:1304-1305.
    ms_log::debugl_hot!(
        ms_log::LOG_SOCKET,
        "start writev handle {}, iiov {}, len {}",
        log_handle,
        chunks.len(),
        len
    );
    // C++ times the write only when LOG_SOCKET is in opts.log.
    let start = ms_log::enabled(ms_log::LOG_SOCKET).then(std::time::Instant::now);
    let slices: Vec<IoSlice<'_>> = chunks.iter().map(|chunk| IoSlice::new(chunk)).collect();
    let result = sink.write_vectored(&slices);
    let elapsed = start
        .map(|start| i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    match result {
        Ok(written) => {
            if written != len {
                // socketworks.cpp:1324-1329. errno is stale on a short write.
                ms_log::debugl_hot!(
                    ms_log::LOG_SOCKET,
                    "writev handle {}, iiov {}, len {}, rv {}, errno {}",
                    log_handle,
                    chunks.len(),
                    len,
                    written,
                    0
                );
            }
            // socketworks.cpp:1352-1353.
            ms_log::debugl_hot!(
                ms_log::LOG_SOCKET,
                "writev returned {} handle {}, iiov {}, len {} (took {} ms)",
                written,
                log_handle,
                chunks.len(),
                len,
                elapsed
            );
            Ok(Some(written))
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            // socketworks.cpp:1324-1329, then the early -EWOULDBLOCK return.
            ms_log::debugl_hot!(
                ms_log::LOG_SOCKET,
                "writev handle {}, iiov {}, len {}, rv {}, errno {}",
                log_handle,
                chunks.len(),
                len,
                -1,
                libc::EWOULDBLOCK
            );
            Ok(None)
        }
        Err(error) => {
            let errno = ms_log::errno(&error);
            // socketworks.cpp:1324-1329.
            ms_log::debugl_hot!(
                ms_log::LOG_SOCKET,
                "writev handle {}, iiov {}, len {}, rv {}, errno {}",
                log_handle,
                chunks.len(),
                len,
                -1,
                errno
            );
            if errno == libc::ECONNREFUSED || errno == libc::EPIPE {
                // socketworks.cpp:1334-1343.
                ms_log::log!(
                    "Connection REFUSED on handle {}, socket {} (sid {}), closing the socket, remote {}:{}",
                    log_handle,
                    log_id,
                    log_sid,
                    "",
                    0
                );
            }
            // socketworks.cpp:1345-1348.
            ms_log::log!(
                "writev returned {} handle {}, iiov {} errno {} error {}",
                -1,
                log_handle,
                chunks.len(),
                errno,
                ms_log::strerror(&error)
            );
            // socketworks.cpp:1352-1353.
            ms_log::debugl_hot!(
                ms_log::LOG_SOCKET,
                "writev returned {} handle {}, iiov {}, len {} (took {} ms)",
                -1,
                log_handle,
                chunks.len(),
                len,
                elapsed
            );
            Err(error)
        }
    }
}
