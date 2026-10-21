//! Ring buffer port of the C++ `SFIFO` used by socketworks.

/// Wraps two slices covering the contiguous region requested from a `Fifo`.
pub type FifoPeek<'a> = (&'a [u8], &'a [u8]);

/// Circular byte buffer with 64-bit read/write indices, like `SFIFO`.
#[derive(Debug, Clone)]
pub struct Fifo {
    data: Vec<u8>,
    read_index: u64,
    write_index: u64,
    /// Creation site, logged by the C++ `SFIFO` diagnostics (`fifo.h:42`).
    origin_file: &'static str,
    origin_line: u32,
}

impl Fifo {
    /// Allocates a zeroed buffer of `size` bytes.
    ///
    /// `#[track_caller]` supplies the nearest Rust equivalent of the
    /// `__FILE__`/`__LINE__` pair `create_fifo` stores in `SFIFO`.
    #[track_caller]
    pub fn new(size: usize) -> Self {
        let origin = std::panic::Location::caller();
        Self {
            data: vec![0; size],
            read_index: 0,
            write_index: 0,
            origin_file: origin.file(),
            origin_line: origin.line(),
        }
    }

    /// Capacity in bytes.
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Logical read position.
    pub fn read_index(&self) -> u64 {
        self.read_index
    }

    /// Logical write position.
    pub fn write_index(&self) -> u64 {
        self.write_index
    }

    /// Replaces both indices; used by the ported C++ tests.
    pub fn set_indices(&mut self, read_index: u64, write_index: u64) {
        self.read_index = read_index;
        self.write_index = write_index;
    }

    /// Bytes between the read and write indices.
    pub fn used(&self) -> u64 {
        self.write_index.saturating_sub(self.read_index)
    }

    /// Free space, clamped at zero when force-pushed data overflowed.
    pub fn available(&self) -> usize {
        self.size()
            .saturating_sub(self.used().min(self.size() as u64) as usize)
    }

    /// Raw backing storage.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Raw backing storage, for tests that seed bytes at an index directly.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Pushes bytes only when they fit.
    pub fn push(&mut self, src: &[u8]) -> usize {
        self.push_force(src, false)
    }

    /// Pushes bytes, optionally overwriting unread data.
    pub fn push_force(&mut self, src: &[u8], force: bool) -> usize {
        if self.size() == 0 || src.len() > self.size() {
            return 0;
        }
        if !force && src.len() > self.available() {
            // fifo.cpp:64
            ms_log::log!(
                "Not enough space in FIFO created at {}:{}: need {}, available {}",
                self.origin_file,
                self.origin_line,
                src.len(),
                self.available()
            );
            return 0;
        }
        if self.read_index > 0 && src.len() > self.available() {
            // fifo.cpp:70 (`%d` is the unsigned `len - available`)
            ms_log::log!(
                "Overwriting {} bytes in the FIFO created at {}:{}",
                src.len() - self.available(),
                self.origin_file,
                self.origin_line
            );
        }
        let offset = (self.write_index % self.size() as u64) as usize;
        let first = src.len().min(self.size() - offset);
        self.data[offset..offset + first].copy_from_slice(&src[..first]);
        if src.len() > first {
            self.data[..src.len() - first].copy_from_slice(&src[first..]);
        }
        self.write_index = self.write_index.wrapping_add(src.len() as u64);
        src.len()
    }

    /// Returns one or two slices covering up to `len` bytes at a relative offset.
    pub fn peek(&self, len: usize, relative_offset: u64) -> FifoPeek<'_> {
        let size = self.size();
        if size == 0 {
            return (&[], &[]);
        }
        let start = self.read_index.saturating_add(relative_offset);
        if start > self.write_index {
            return (&[], &[]);
        }
        let len = len.min((self.write_index - start) as usize);
        let offset = (start % size as u64) as usize;
        let first = len.min(size - offset);
        (
            &self.data[offset..offset + first],
            &self.data[..len - first],
        )
    }

    /// Reads four bytes at an absolute logical position, zero when short.
    pub fn peek_32(&self, offset: u64) -> u32 {
        let size = self.size();
        if size == 0 || self.write_index.saturating_sub(offset) < 4 {
            return 0;
        }
        let mut value = 0u32;
        for i in 0..4u64 {
            value = (value << 8) | self.data[((offset + i) % size as u64) as usize] as u32;
        }
        value
    }

    /// Advances the read index.
    pub fn skip(&mut self, len: u64) {
        if self.size() > 0 {
            self.read_index = self.read_index.wrapping_add(len);
        }
    }
}

/// Finds the next RTSP-over-TCP interleaved frame header relative to the read index.
pub fn find_next_rtsp_header(fifo: &Fifo) -> Option<u64> {
    if fifo.write_index < 8 {
        return None;
    }
    let start = fifo.read_index;
    let end = fifo.write_index - 8;
    let mut offset = start;
    while offset < end {
        let first = fifo.peek_32(offset) & 0xFFFF_0000;
        let second = fifo.peek_32(offset + 4) & 0xFFFF_0000;
        if (first == 0x2400_0000 && second == 0x8021_0000)
            || (first == 0x2401_0000 && second == 0x80C8_0000)
        {
            return Some(offset - start);
        }
        offset += 1;
    }
    None
}

/// Copies the iovec contents starting at `offset` into the FIFO.
pub fn copy_iovec_to_fifo(fifo: &mut Fifo, offset: usize, chunks: &[&[u8]]) -> usize {
    let len: usize = chunks.iter().map(|chunk| chunk.len()).sum();
    if len.saturating_sub(offset) > fifo.available() {
        return 0;
    }
    let before = fifo.available();
    let mut offset = offset;
    for chunk in chunks {
        if offset < chunk.len() {
            fifo.push(&chunk[offset..]);
        }
        offset = if offset > chunk.len() {
            offset - chunk.len()
        } else {
            0
        };
    }
    before - fifo.available()
}
