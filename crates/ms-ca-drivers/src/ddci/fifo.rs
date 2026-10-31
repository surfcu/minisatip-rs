//! Byte FIFO with one read index per reader (`SFIFO`, `fifo_push`, `fifo_pop_offset`).

/// Ring buffer addressed by monotonic indices.
#[derive(Clone, Debug)]
pub struct Fifo {
    data: Vec<u8>,
    pub write_index: u64,
    /// `SFIFO::file`/`SFIFO::line`, reported by `fifo_pop_offset` (fifo.cpp:93).
    origin_file: &'static str,
    origin_line: u32,
}

impl Fifo {
    /// `#[track_caller]` supplies the nearest Rust equivalent of the
    /// `__FILE__`/`__LINE__` pair `create_fifo` stores in `SFIFO`.
    #[track_caller]
    pub fn new(size: usize) -> Self {
        let origin = std::panic::Location::caller();
        Self {
            data: vec![0; size],
            write_index: 0,
            origin_file: origin.file(),
            origin_line: origin.line(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    /// The backing ring buffer (`d->fifo.data`), for the debug packet dumps.
    pub fn raw_data(&self) -> &[u8] {
        &self.data
    }

    /// Appends, overwriting the oldest bytes when full (`fifo_push_force`).
    pub fn push(&mut self, src: &[u8]) -> usize {
        let cap = self.data.len() as u64;
        if cap == 0 {
            return 0;
        }
        for &b in src {
            self.data[(self.write_index % cap) as usize] = b;
            self.write_index += 1;
        }
        src.len()
    }

    /// Copies up to `dst.len()` bytes from `read_index`, advancing it.
    pub fn pop_offset(&self, dst: &mut [u8], read_index: &mut u64, adapter: i64) -> usize {
        let cap = self.data.len() as u64;
        if *read_index + cap < self.write_index {
            // fifo.cpp:91-97
            if *read_index > 0 {
                let lost = self.write_index - cap - *read_index;
                ms_log::log!(
                    "Lost {} bytes in fifo created at {}:{}",
                    lost,
                    self.origin_file,
                    self.origin_line
                );
                ms_log::diag::count_fifo_overflow(adapter, lost);
            }
            *read_index = self.write_index - cap;
        }
        let avail = self.write_index.saturating_sub(*read_index) as usize;
        let n = avail.min(dst.len());
        for (i, d) in dst.iter_mut().take(n).enumerate() {
            *d = self.data[((*read_index + i as u64) % cap) as usize];
        }
        *read_index += n as u64;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::Fifo;

    // Ported from tests/test_utils.cpp:76 (test_fifo, f3); the push cases test
    // the output FIFO in ms-net (tests/ported_test_utils.rs).
    #[test]
    fn test_fifo_pop_offset() {
        let mut f3 = Fifo::new(100);
        f3.push(&[0u8; 50]);
        let mut offset = 30;
        let mut out = [0u8; 50];
        assert_eq!(
            f3.pop_offset(&mut out, &mut offset, 0),
            20,
            "Expected fifo_pop_offset to cap to write_index - offset (20 bytes)"
        );
        assert_eq!(offset, 50, "Expected offset to advance to write_index (50)");
    }
}
