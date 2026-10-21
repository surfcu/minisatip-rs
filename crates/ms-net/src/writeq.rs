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
        }
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
        if self.fifo.used() == 0 {
            match write_chunks(sink, chunks) {
                Ok(None) => {}
                Ok(Some(written)) => {
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
            self.overflow = self.overflow.saturating_add((len - offset) as u64);
            return 0;
        }
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
            self.fifo.set_indices(read_index, read_index + offset);
        }
        self.flush_enqueued_data = false;
    }

    fn flush_all<W: Write + ?Sized>(&mut self, sink: &mut W, used: usize) -> i32 {
        if used == 0 {
            return 0;
        }
        let result = {
            let (first, second) = self.fifo.peek(used, 0);
            let chunks: [&[u8]; 2] = [first, second];
            let count = if second.is_empty() { 1 } else { 2 };
            write_chunks(sink, &chunks[..count])
        };
        match result {
            Ok(None) => 0,
            Ok(Some(written)) if written > 0 => {
                self.fifo.skip(written as u64);
                written as i32
            }
            Ok(Some(_)) => 0,
            Err(_) => -1,
        }
    }

    fn flush_prio<W: Write + ?Sized>(&mut self, sink: &mut W) -> i32 {
        let mut left = find_next_rtsp_header(&self.fifo)
            .map(|offset| offset as i64)
            .unwrap_or(-1);
        if left > 0 {
            let written = self.flush_all(sink, left as usize);
            if written >= 0 {
                left -= written as i64;
            }
        }
        if left > 0 {
            return left as i32;
        }

        let prio_len = self.prio_data.len();
        let result = write_chunks(sink, &[&self.prio_data]);
        match result {
            Ok(None) | Ok(Some(0)) | Err(_) => 1,
            Ok(Some(written)) if written != prio_len => {
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

fn write_chunks<W: Write + ?Sized>(sink: &mut W, chunks: &[&[u8]]) -> io::Result<Option<usize>> {
    let slices: Vec<IoSlice<'_>> = chunks.iter().map(|chunk| IoSlice::new(chunk)).collect();
    match sink.write_vectored(&slices) {
        Ok(written) => Ok(Some(written)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}
