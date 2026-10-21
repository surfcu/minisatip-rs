//! In-place processing stage between PID routing and the outputs.

/// Rewrites an adapter read buffer before it reaches the outputs.
/// The data plane calls it once per batch, like the C++ `pmt_decrypt_stream`.
pub trait TsStage: Send {
    /// Processes `buf`, a whole number of 188-byte packets, in place.
    fn process(&mut self, buf: &mut [u8]);
}
