//! Software descrambling: CW slots, parity batches and the CSA/AES ciphers.
//! `pipeline` ports the `pmt.cpp` decrypt path; CSA links libdvbcsa behind `dvbcsa`.

pub mod aes;
pub mod cipher;
#[cfg(feature = "dvbcsa")]
pub mod csa;
pub mod pipeline;
pub mod stage;

#[cfg(test)]
mod tests;

pub use cipher::{AesCbc, AesEcb, Cipher, CipherFactory, CipherRegistry, KeyMaterial};
pub use pipeline::{
    AdapterPid, DVB_FRAME, Descrambler, MAX_CW, MAX_CW_TIME, PidCounters, SendCwError,
    ServiceStats, adaptation_len, test_decrypt_packets,
};
pub use stage::{DescramblerCommand, DescramblerStage, DescramblerStats, StageConfig};
