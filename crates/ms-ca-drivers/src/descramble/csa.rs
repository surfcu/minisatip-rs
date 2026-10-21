//! DVB-CSA through libdvbcsa's bitslice API (`csa.cpp`); the only `unsafe` code.
#![allow(unsafe_code)]

use std::ffi::c_uint;
use std::marker::{PhantomData, PhantomPinned};

use ms_ca::CwAlgo;

use super::cipher::{Cipher, CipherRegistry, KeyMaterial};

/// Longest payload the C++ hands to `dvbcsa_bs_decrypt`.
const MAX_PAYLOAD: c_uint = 184;

#[repr(C)]
struct BsKey {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

#[repr(C)]
struct BsBatch {
    data: *mut u8,
    len: c_uint,
}

// SAFETY: declarations from `dvbcsa/dvbcsa.h`; every call site below passes
// live keys and buffers of the sizes the library documents.
unsafe extern "C" {
    fn dvbcsa_bs_key_alloc() -> *mut BsKey;
    fn dvbcsa_bs_key_free(key: *mut BsKey);
    fn dvbcsa_bs_key_set(cw: *const u8, key: *mut BsKey);
    fn dvbcsa_bs_batch_size() -> c_uint;
    fn dvbcsa_bs_decrypt(key: *const BsKey, pcks: *const BsBatch, maxlen: c_uint);
    fn dvbcsa_bs_encrypt(key: *const BsKey, pcks: *const BsBatch, maxlen: c_uint);
    #[cfg(feature = "dvbcsa-icam")]
    fn dvbcsa_bs_key_set_ecm(ecm: u8, cw: *const u8, key: *mut BsKey);
}

/// Registers CSA, and iCAM CSA when libdvbcsa has `dvbcsa_bs_key_set_ecm`.
pub(super) fn register(registry: &mut CipherRegistry) {
    registry.register(CwAlgo::Csa, || Box::new(Csa::new(false)));
    #[cfg(feature = "dvbcsa-icam")]
    registry.register(CwAlgo::CsaIcam, || Box::new(Csa::new(true)));
}

/// One bitslice CSA key (`dvbcsa_create_key`).
pub struct Csa {
    key: *mut BsKey,
    icam: bool,
}

// SAFETY: the key is heap memory owned only by this value; libdvbcsa keeps no
// references to it and has no thread affinity.
unsafe impl Send for Csa {}

impl Csa {
    /// Allocates a key; `icam` selects `dvbcsa_bs_key_set_ecm` in `set_cw`.
    pub fn new(icam: bool) -> Self {
        // SAFETY: plain allocation without arguments; the result is checked on use.
        let key = unsafe { dvbcsa_bs_key_alloc() };
        assert!(!key.is_null(), "dvbcsa_bs_key_alloc failed");
        Self { key, icam }
    }

    fn run(&self, payloads: &mut [&mut [u8]], encrypt: bool) {
        // SAFETY: no arguments; returns a constant of the library.
        let size = unsafe { dvbcsa_bs_batch_size() }.max(1) as usize;
        let mut batch = Vec::with_capacity(size + 1);
        for chunk in payloads.chunks_mut(size) {
            batch.clear();
            batch.extend(chunk.iter_mut().map(|p| BsBatch {
                data: p.as_mut_ptr(),
                len: p.len().min(MAX_PAYLOAD as usize) as c_uint,
            }));
            batch.push(BsBatch {
                data: std::ptr::null_mut(),
                len: 0,
            });
            // SAFETY: at most batch_size entries, NULL-terminated; each points to
            // `len` writable bytes borrowed mutably for this call only.
            unsafe {
                if encrypt {
                    dvbcsa_bs_encrypt(self.key, batch.as_ptr(), MAX_PAYLOAD);
                } else {
                    dvbcsa_bs_decrypt(self.key, batch.as_ptr(), MAX_PAYLOAD);
                }
            }
        }
    }

    /// Encrypts payloads with the loaded key (test scrambler).
    pub fn encrypt(&mut self, payloads: &mut [&mut [u8]]) {
        self.run(payloads, true);
    }
}

impl Cipher for Csa {
    fn set_cw(&mut self, key: &KeyMaterial) {
        let cw = key.cw.as_ptr();
        #[cfg(feature = "dvbcsa-icam")]
        if self.icam {
            // SAFETY: `cw` points to at least 8 readable bytes; `self.key` is live.
            unsafe { dvbcsa_bs_key_set_ecm(key.icam_ecm, cw, self.key) };
            return;
        }
        let _ = self.icam;
        // SAFETY: `cw` points to at least 8 readable bytes; `self.key` is live.
        unsafe { dvbcsa_bs_key_set(cw, self.key) };
    }

    fn decrypt(&mut self, _key: &KeyMaterial, payloads: &mut [&mut [u8]]) {
        self.run(payloads, false);
    }
}

impl Drop for Csa {
    fn drop(&mut self) {
        // SAFETY: allocated by dvbcsa_bs_key_alloc and freed exactly once.
        unsafe { dvbcsa_bs_key_free(self.key) };
    }
}
