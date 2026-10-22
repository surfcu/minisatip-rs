//! In-place processing stage between PID routing and the outputs.

/// Rewrites an adapter read buffer before it reaches the outputs.
/// The data plane calls it once per batch, like the C++ `pmt_decrypt_stream`.
pub trait TsStage: Send {
    /// Processes `buf`, a whole number of 188-byte packets, in place.
    fn process(&mut self, buf: &mut [u8]);

    /// Like [`Self::process`], but the stage may append whole packets until
    /// `buf` holds `capacity` bytes (C++ stages grow `ad->rlen` up to `ad->lbuf`).
    fn process_buf(&mut self, buf: &mut Vec<u8>, capacity: usize) {
        let _ = capacity;
        self.process(buf);
    }
}
