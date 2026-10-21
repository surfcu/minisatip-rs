//! Cipher backends behind one trait (the C++ `SCW_op` table).

use ms_ca::CwAlgo;

use super::aes::{Aes128, BLOCK};

/// Most registered algorithms (`MAX_OPS`).
pub const MAX_OPS: usize = 10;

/// Key material of one CW slot (the `SCW` key fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyMaterial {
    /// Key bytes; the first `cw_len` are significant.
    pub cw: [u8; 32],
    /// IV bytes (zero when the CA sent none).
    pub iv: [u8; 32],
    /// 8 for CSA/DES/iCAM, 16 otherwise.
    pub cw_len: usize,
    /// iCAM ECM mode byte (`cw->opaque`).
    pub icam_ecm: u8,
}

impl KeyMaterial {
    fn aes_key(&self) -> [u8; BLOCK] {
        self.cw[..BLOCK].try_into().unwrap_or([0; BLOCK])
    }

    fn aes_iv(&self) -> [u8; BLOCK] {
        self.iv[..BLOCK].try_into().unwrap_or([0; BLOCK])
    }
}

/// One key instance of an algorithm (`create_cw`/`set_cw`/`stop_cw`/`decrypt_stream`).
pub trait Cipher: Send {
    /// Loads a new key (`set_cw`).
    fn set_cw(&mut self, key: &KeyMaterial);
    /// The key is being replaced or dropped (`stop_cw`).
    fn stop_cw(&mut self, _key: &KeyMaterial) {}
    /// Decrypts each TS payload in place (`decrypt_stream`).
    fn decrypt(&mut self, key: &KeyMaterial, payloads: &mut [&mut [u8]]);
}

/// Creates a key instance (`create_cw`).
pub type CipherFactory = fn() -> Box<dyn Cipher>;

/// Registered algorithms in priority order (`register_algo`/`get_op_for_algo`).
#[derive(Clone, Default)]
pub struct CipherRegistry {
    ops: Vec<(CwAlgo, CipherFactory)>,
}

impl CipherRegistry {
    /// An empty registry.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The software algorithms of this build, like `init_algo` without hardware.
    pub fn software() -> Self {
        let mut registry = Self::empty();
        #[cfg(feature = "dvbcsa")]
        super::csa::register(&mut registry);
        registry.register(CwAlgo::Aes128Ecb, || Box::new(AesEcb::default()));
        registry.register(CwAlgo::Aes128Cbc, || Box::new(AesCbc));
        registry
    }

    /// Appends an algorithm; `false` when `MAX_OPS` are registered.
    pub fn register(&mut self, algo: CwAlgo, factory: CipherFactory) -> bool {
        if self.ops.len() >= MAX_OPS {
            return false;
        }
        self.ops.push((algo, factory));
        true
    }

    /// The first factory registered for `algo`.
    pub fn get(&self, algo: CwAlgo) -> Option<CipherFactory> {
        self.ops.iter().find(|(a, _)| *a == algo).map(|(_, f)| *f)
    }
}

/// AES-128 ECB (`aes_op`): whole 16-byte blocks of each payload.
#[derive(Default)]
pub struct AesEcb {
    aes: Option<Aes128>,
}

impl Cipher for AesEcb {
    fn set_cw(&mut self, key: &KeyMaterial) {
        self.aes = Some(Aes128::new(&key.aes_key()));
    }

    fn decrypt(&mut self, _key: &KeyMaterial, payloads: &mut [&mut [u8]]) {
        let Some(aes) = &self.aes else { return };
        for payload in payloads.iter_mut() {
            aes.decrypt_ecb(payload);
        }
    }
}

/// AES-128 CBC (`aes_cbc_op`): each payload restarts from the key's IV.
/// C++'s extra `len / 16 + 1` block makes OpenSSL write exactly the whole blocks.
pub struct AesCbc;

impl Cipher for AesCbc {
    fn set_cw(&mut self, _key: &KeyMaterial) {}

    fn decrypt(&mut self, key: &KeyMaterial, payloads: &mut [&mut [u8]]) {
        let aes = Aes128::new(&key.aes_key());
        let iv = key.aes_iv();
        for payload in payloads.iter_mut() {
            aes.decrypt_cbc(&iv, payload);
        }
    }
}
