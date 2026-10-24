//! AES, XCBC-MAC, SHA-256 and Diffie-Hellman helpers from `ca.cpp`.

use openssl::bn::{BigNum, BigNumContext};
use openssl::hash::{Hasher, MessageDigest};
use openssl::pkey::{PKey, Private};
use openssl::sign::{RsaPssSaltlen, Signer};
use openssl::symm::{Cipher, Crypter, Mode};

use super::ffi::wipe;

/// `dh_p`: the 2048-bit Diffie-Hellman prime (ca.cpp:109).
pub const DH_P: [u8; 256] = [
    0xd6, 0x27, 0x14, 0x7a, 0x7c, 0x0c, 0x26, 0x63, 0x9d, 0x82, 0xeb, 0x1f, 0x4a, 0x18, 0xff, 0x6c,
    0x34, 0xad, 0xea, 0xa6, 0xc0, 0x23, 0xe6, 0x65, 0xfc, 0x8e, 0x32, 0xc3, 0x33, 0xf4, 0x91, 0xa7,
    0xcc, 0x88, 0x58, 0xd7, 0xf3, 0xb3, 0x17, 0x5e, 0xb0, 0xa8, 0xeb, 0x5c, 0xd4, 0xd8, 0x3a, 0xae,
    0x8e, 0x75, 0xa1, 0x50, 0x5f, 0x5d, 0x67, 0xc5, 0x40, 0xf4, 0xb3, 0x68, 0x35, 0xd1, 0x3a, 0x4c,
    0x93, 0x7f, 0xca, 0xce, 0xdd, 0x83, 0x29, 0x01, 0xc8, 0x4b, 0x76, 0x81, 0x56, 0x34, 0x83, 0x31,
    0x92, 0x72, 0x65, 0x7b, 0xac, 0xd9, 0xda, 0xa9, 0xd1, 0xd3, 0xe5, 0x77, 0x58, 0x6f, 0x5b, 0x44,
    0x3e, 0xaf, 0x7f, 0x6d, 0xf5, 0xcf, 0x0a, 0x80, 0x0d, 0xa5, 0x56, 0x4f, 0x4b, 0x85, 0x41, 0x0f,
    0x13, 0x41, 0x06, 0x1f, 0xf3, 0xd9, 0x65, 0x36, 0xae, 0x47, 0x41, 0x1f, 0x1f, 0xe0, 0xde, 0x69,
    0xe5, 0x86, 0x2a, 0xa1, 0xf2, 0x48, 0x02, 0x92, 0x68, 0xa6, 0x37, 0x9f, 0x76, 0x4f, 0x7d, 0x94,
    0x5d, 0x10, 0xe5, 0xab, 0x5d, 0xb2, 0xf3, 0x12, 0x8c, 0x79, 0x03, 0x92, 0xa6, 0x7f, 0x8a, 0x78,
    0xb0, 0xba, 0xc5, 0xb5, 0x31, 0xc5, 0xc8, 0x22, 0x6e, 0x29, 0x02, 0x40, 0xab, 0xe7, 0x5c, 0x23,
    0x33, 0x7f, 0xcb, 0x86, 0xc7, 0xb4, 0xfd, 0xaa, 0x44, 0xcd, 0x9c, 0x9f, 0xba, 0xac, 0x3a, 0xcf,
    0x7e, 0x31, 0x5f, 0xa8, 0x47, 0xce, 0xca, 0x1c, 0xb4, 0x77, 0xa0, 0xec, 0x9a, 0x46, 0xd4, 0x79,
    0x7b, 0x64, 0xbb, 0x6c, 0x91, 0xb2, 0x38, 0x01, 0x65, 0x11, 0x45, 0x9f, 0x62, 0x08, 0x6f, 0x31,
    0xcf, 0xc4, 0xba, 0xdc, 0xd0, 0x03, 0x91, 0xf1, 0x18, 0x1f, 0xcb, 0x4d, 0xfc, 0x73, 0x5a, 0xa2,
    0x15, 0xb8, 0x3c, 0x8d, 0x80, 0x92, 0x1c, 0xa1, 0x03, 0xd0, 0x83, 0x2f, 0x5f, 0xe3, 0x07, 0x69,
];

/// `dh_g`: the 2048-bit Diffie-Hellman generator (ca.cpp:134).
pub const DH_G: [u8; 256] = [
    0x95, 0x7d, 0xd1, 0x49, 0x68, 0xc1, 0xa5, 0xf1, 0x48, 0xe6, 0x50, 0x4f, 0xa1, 0x10, 0x72, 0xc4,
    0xef, 0x12, 0xec, 0x2d, 0x94, 0xbe, 0xc7, 0x20, 0x2c, 0x94, 0xf9, 0x68, 0x67, 0x0e, 0x22, 0x17,
    0xb5, 0x5c, 0x0b, 0xca, 0xac, 0x9f, 0x25, 0x9c, 0xd2, 0xa6, 0x1a, 0x20, 0x10, 0x16, 0x6a, 0x42,
    0x27, 0x83, 0x47, 0x42, 0xa0, 0x07, 0x52, 0x09, 0x33, 0x97, 0x4e, 0x30, 0x57, 0xd8, 0xb7, 0x1e,
    0x46, 0xa6, 0xba, 0x4e, 0x40, 0x6a, 0xe9, 0x1a, 0x5a, 0xa0, 0x74, 0x56, 0x92, 0x55, 0xc2, 0xbd,
    0x44, 0xcd, 0xb3, 0x33, 0xf7, 0x35, 0x46, 0x25, 0xdf, 0x84, 0x19, 0xf3, 0xe2, 0x7a, 0xac, 0x4e,
    0xee, 0x1a, 0x86, 0x3b, 0xb3, 0x87, 0xa6, 0x66, 0xc1, 0x70, 0x21, 0x41, 0xd3, 0x58, 0x36, 0xb5,
    0x3b, 0x6e, 0xa1, 0x55, 0x60, 0x9a, 0x59, 0xd3, 0x85, 0xd8, 0xdc, 0x6a, 0xff, 0x41, 0xb6, 0xbf,
    0x42, 0xde, 0x64, 0x00, 0xd0, 0xee, 0x3a, 0xa1, 0x8a, 0xed, 0x12, 0xf9, 0xba, 0x54, 0x5c, 0xdb,
    0x06, 0x24, 0x49, 0xe8, 0x47, 0xcf, 0x5b, 0xe4, 0xbb, 0xc0, 0xaa, 0x8a, 0x8c, 0xbe, 0x73, 0xd9,
    0x02, 0xea, 0xee, 0x8d, 0x87, 0x5b, 0xbf, 0x78, 0x04, 0x41, 0x9e, 0xa8, 0x5c, 0x3c, 0x49, 0xde,
    0x88, 0x6d, 0x62, 0x21, 0x7f, 0xf0, 0x5e, 0x2d, 0x1d, 0xfc, 0x47, 0x0d, 0x1b, 0xaa, 0x4e, 0x0d,
    0x78, 0x20, 0xfe, 0x57, 0x0f, 0xca, 0xdf, 0xeb, 0x3c, 0x84, 0xa7, 0xe1, 0x61, 0xb2, 0x95, 0x98,
    0x07, 0x73, 0x8e, 0x51, 0xc6, 0x87, 0xe4, 0xcf, 0xf1, 0x5f, 0x86, 0x99, 0xec, 0x8d, 0x44, 0x92,
    0x2c, 0x99, 0xf6, 0xc0, 0xf4, 0x39, 0xe8, 0x05, 0xbf, 0xc1, 0x56, 0xde, 0xfe, 0x93, 0x75, 0x06,
    0x69, 0x87, 0x83, 0x06, 0x51, 0x80, 0xa5, 0x6e, 0xa6, 0x19, 0x7d, 0x3b, 0xef, 0xfb, 0xe0, 0x4a,
];

/// SAC cipher IV (`sac_crypt`, ca.cpp:1862).
const SAC_IV: [u8; 16] = [
    0xf7, 0x70, 0xb0, 0x36, 0x03, 0x61, 0xf7, 0x96, 0x65, 0x74, 0x8a, 0x26, 0xea, 0x4e, 0x85, 0x41,
];

/// `key_data` in `generate_SAK_SEK` (ca.cpp:1841).
const SAK_SEK_KEY: [u8; 16] = [
    0xea, 0x74, 0xf4, 0x71, 0x99, 0xd7, 0x6f, 0x35, 0x89, 0xf0, 0xd1, 0xdf, 0x0f, 0xee, 0xe3, 0x00,
];

/// `s_key` in `check_new_key` (ca.cpp:2102).
const PRECURSOR_KEY: [u8; 16] = [
    0x3e, 0x20, 0x15, 0x84, 0x2c, 0x37, 0xce, 0xe3, 0xd6, 0x14, 0x57, 0x3e, 0x3a, 0xab, 0x91, 0xb6,
];

/// Source of random bytes; tests use a fixed implementation.
pub trait Rng {
    fn fill(&mut self, buf: &mut [u8]) -> bool;
}

/// `RAND_bytes` through the OpenSSL crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsRng;

impl Rng for OsRng {
    fn fill(&mut self, buf: &mut [u8]) -> bool {
        openssl::rand::rand_bytes(buf).is_ok()
    }
}

fn aes_ecb(key: &[u8; 16], block: &[u8; 16]) -> Option<[u8; 16]> {
    let mut crypter = Crypter::new(Cipher::aes_128_ecb(), Mode::Encrypt, key, None).ok()?;
    crypter.pad(false);
    let mut out = [0u8; 32];
    let first = crypter.update(block, &mut out).ok()?;
    let last = crypter.finalize(&mut out[first..]).ok()?;
    if first + last != 16 {
        return None;
    }
    out[..16].try_into().ok()
}

/// `aes_ecb_encrypt_block`: one AES-128-ECB block without padding. A failed
/// OpenSSL call yields zeros (unreachable for a 16-byte block).
pub fn aes_ecb_encrypt_block(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    aes_ecb(key, block).unwrap_or([0u8; 16])
}

fn aes_cbc(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8], mode: Mode) {
    let len = data.len() & !15;
    if len == 0 {
        return;
    }
    let Ok(mut crypter) = Crypter::new(Cipher::aes_128_cbc(), mode, key, Some(iv)) else {
        return;
    };
    crypter.pad(false);
    let mut out = vec![0u8; len + 16];
    let done = (|| {
        let first = crypter.update(&data[..len], &mut out).ok()?;
        let last = crypter.finalize(&mut out[first..]).ok()?;
        (first + last == len).then_some(())
    })();
    if done.is_some() {
        data[..len].copy_from_slice(&out[..len]);
    }
    wipe(&mut out);
}

/// `aes_cbc_encrypt`: AES-128-CBC without padding, in place. Only whole
/// 16-byte blocks are processed; a trailing partial block is left untouched.
pub fn aes_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    aes_cbc(key, iv, data, Mode::Encrypt);
}

/// `aes_cbc_decrypt`: AES-128-CBC without padding, in place. Only whole
/// 16-byte blocks are processed; a trailing partial block is left untouched.
pub fn aes_cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    aes_cbc(key, iv, data, Mode::Decrypt);
}

/// `sha256_init`/`sha256_update`/`sha256_final` over consecutive parts.
pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let Ok(mut hasher) = Hasher::new(MessageDigest::sha256()) else {
        return out;
    };
    for part in parts {
        if hasher.update(part).is_err() {
            return [0u8; 32];
        }
    }
    if let Ok(digest) = hasher.finish() {
        let len = digest.len().min(out.len());
        out[..len].copy_from_slice(&digest[..len]);
    }
    out
}

/// `aes_xcbc_mac_init`/`_process`/`_done`: AES-XCBC-MAC as used for the SAC.
#[derive(Clone)]
pub struct AesXcbcMac {
    k: [[u8; 16]; 3],
    iv: [u8; 16],
    buffered: usize,
}

impl Drop for AesXcbcMac {
    fn drop(&mut self) {
        for block in &mut self.k {
            wipe(block);
        }
        wipe(&mut self.iv);
    }
}

impl AesXcbcMac {
    /// `aes_xcbc_mac_init`: derives K1..K3 from the key.
    pub fn new(key: &[u8; 16]) -> Self {
        let mut k = [[0u8; 16]; 3];
        for (i, block) in k.iter_mut().enumerate() {
            *block = [i as u8 + 1; 16];
            *block = aes_ecb_encrypt_block(key, block);
        }
        Self {
            k,
            iv: [0u8; 16],
            buffered: 0,
        }
    }

    /// `aes_xcbc_mac_process`: a full buffer is only flushed when the next
    /// byte arrives, so `finish` can still tell a full buffer from padding.
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            if self.buffered == 16 {
                self.iv = aes_ecb_encrypt_block(&self.k[0], &self.iv);
                self.buffered = 0;
            }
            self.iv[self.buffered] ^= byte;
            self.buffered += 1;
        }
    }

    /// `aes_xcbc_mac_done`: K2 for a full buffer, otherwise 0x80 and K3.
    pub fn finish(mut self) -> [u8; 16] {
        if self.buffered == 16 {
            for i in 0..16 {
                self.iv[i] ^= self.k[1][i];
            }
        } else {
            self.iv[self.buffered] ^= 0x80;
            for i in 0..16 {
                self.iv[i] ^= self.k[2][i];
            }
        }
        aes_ecb_encrypt_block(&self.k[0], &self.iv)
    }
}

/// `dh_gen_exp`: 256 random exponent bytes. A failed RNG leaves zeros; the
/// OpenSSL RNG does not fail in practice and callers cannot distinguish it.
pub fn dh_gen_exp(rng: &mut dyn Rng) -> [u8; 256] {
    let mut out = [0u8; 256];
    rng.fill(&mut out);
    out
}

/// `dh_mod_exp`: `base ^ exp mod modulus`, left-padded to `out_len`.
pub fn dh_mod_exp(base: &[u8], exp: &[u8], modulus: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let base = BigNum::from_slice(base).ok()?;
    let exp = BigNum::from_slice(exp).ok()?;
    let modulus = BigNum::from_slice(modulus).ok()?;
    let mut ctx = BigNumContext::new().ok()?;
    let mut result = BigNum::new().ok()?;
    result.mod_exp(&base, &exp, &modulus, &mut ctx).ok()?;
    let mut bytes = result.to_vec();
    if bytes.len() > out_len {
        wipe(&mut bytes);
        return None;
    }
    let mut out = vec![0u8; out_len - bytes.len()];
    out.extend_from_slice(&bytes);
    wipe(&mut bytes);
    Some(out)
}

/// `dh_dhph_signature`: RSA-PSS/SHA-1 (MGF1-SHA1, salt 20) over the
/// 0x12e-byte authentication message built here.
pub fn dhph_signature(
    nonce: &[u8; 32],
    dhph: &[u8; 256],
    key: &PKey<Private>,
) -> Option<[u8; 256]> {
    let mut msg = [0u8; 0x12e];
    msg[0x00] = 0x00; // version
    msg[0x01] = 0x00;
    msg[0x02] = 0x08; // len (bits)
    msg[0x03] = 0x01; // version data
    msg[0x04] = 0x01; // msg_label
    msg[0x05] = 0x00;
    msg[0x06] = 0x08; // len (bits)
    msg[0x07] = 0x02; // message data
    msg[0x08] = 0x02; // auth_nonce
    msg[0x09] = 0x01;
    msg[0x0a] = 0x00; // len (bits)
    msg[0x0b..0x2b].copy_from_slice(nonce);
    msg[0x2b] = 0x04; // DHPH - DH public key host
    msg[0x2c] = 0x08;
    msg[0x2d] = 0x00; // len (bits)
    msg[0x2e..].copy_from_slice(dhph);

    let mut signer = Signer::new(MessageDigest::sha1(), key).ok()?;
    signer
        .set_rsa_padding(openssl::rsa::Padding::PKCS1_PSS)
        .ok()?;
    signer.set_rsa_pss_saltlen(RsaPssSaltlen::custom(20)).ok()?;
    signer.set_rsa_mgf1_md(MessageDigest::sha1()).ok()?;
    signer.update(&msg).ok()?;
    signer.sign_to_vec().ok()?.try_into().ok()
}

/// `add_padding`: 0x80 then zeros until the length counted from `len_from`
/// is a multiple of `block`; nothing when already aligned.
pub fn add_padding(buf: &mut Vec<u8>, len_from: usize, block: usize) {
    if block == 0 || (buf.len().saturating_sub(len_from)).is_multiple_of(block) {
        return;
    }
    buf.push(0x80);
    while !(buf.len().saturating_sub(len_from)).is_multiple_of(block) {
        buf.push(0);
    }
}

/// `get_bin_from_nibble`: one hex nibble, unsupported characters become 0.
fn get_bin_from_nibble(nibble: u8) -> u8 {
    match nibble {
        b'0'..=b'9' => nibble - b'0',
        b'A'..=b'Z' => nibble - b'A' + 10,
        b'a'..=b'z' => nibble - b'a' + 10,
        _ => {
            ms_log::log!("fixme: unsupported chars in hostid");
            0
        }
    }
}

/// `str2bin`: decodes pairs of hex nibbles into bytes.
pub fn str2bin(data: &[u8]) -> Vec<u8> {
    data.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (get_bin_from_nibble(pair[0]) << 4) | get_bin_from_nibble(pair[1]))
        .collect()
}

/// `generate_SAK_SEK`: derives `(sak, sek)` from the 32-byte key seed.
pub fn generate_sak_sek(ks_host: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut first = [0u8; 16];
    let mut second = [0u8; 16];
    first.copy_from_slice(&ks_host[..16]);
    second.copy_from_slice(&ks_host[16..]);
    let dec_first = aes_ecb_encrypt_block(&SAK_SEK_KEY, &first);
    let dec_second = aes_ecb_encrypt_block(&SAK_SEK_KEY, &second);

    let mut sek = [0u8; 16];
    let mut sak = [0u8; 16];
    for i in 0..16 {
        sek[i] = ks_host[i] ^ dec_first[i];
        sak[i] = ks_host[16 + i] ^ dec_second[i];
    }
    (sak, sek)
}

/// `sac_crypt`: AES-128-CBC with the fixed SAC IV, in place.
pub fn sac_crypt(key: &[u8; 16], data: &mut [u8], encrypt: bool) {
    if encrypt {
        aes_cbc_encrypt(key, &SAC_IV, data);
    } else {
        aes_cbc_decrypt(key, &SAC_IV, data);
    }
}

/// The AES/XOR part of `check_new_key`: `(key, iv)` from a key precursor.
pub fn key_from_precursor(kp: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut first = [0u8; 16];
    let mut second = [0u8; 16];
    first.copy_from_slice(&kp[..16]);
    second.copy_from_slice(&kp[16..]);
    let dec_first = aes_ecb_encrypt_block(&PRECURSOR_KEY, &first);
    let dec_second = aes_ecb_encrypt_block(&PRECURSOR_KEY, &second);

    let mut key = [0u8; 16];
    let mut iv = [0u8; 16];
    for i in 0..16 {
        key[i] = dec_first[i] ^ kp[i];
        iv[i] = dec_second[i] ^ kp[16 + i];
    }
    (key, iv)
}

/// `sac_gen_auth`: MAC over the header length byte and `data`.
pub fn sac_gen_auth(sak: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let mut mac = AesXcbcMac::new(sak);
    mac.update(&[0x04]);
    mac.update(data);
    mac.finish()
}

/// `sac_check_auth`: compares the trailing 16-byte MAC; false when shorter.
pub fn sac_check_auth(sak: &[u8; 16], data: &[u8]) -> bool {
    if data.len() < 16 {
        ms_log::log!("auth too short");
        return false;
    }
    let (body, signature) = data.split_at(data.len() - 16);
    if sac_gen_auth(sak, body) != signature {
        ms_log::log!("signature wrong");
        return false;
    }
    ms_log::log!("auth ok!");
    true
}

/// `generate_uri_confirm`: `sha256(uri_msg || sha256(sak))`.
pub fn uri_confirm(uri_msg: &[u8], sak: &[u8; 16]) -> [u8; 32] {
    let uck = sha256(&[sak]);
    sha256(&[uri_msg, &uck])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(n: usize) -> Vec<u8> {
        (0..n).map(|i| i as u8).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn xcbc(key: &[u8; 16], parts: &[&[u8]]) -> [u8; 16] {
        let mut mac = AesXcbcMac::new(key);
        for part in parts {
            mac.update(part);
        }
        mac.finish()
    }

    #[test]
    fn xcbc_mac_rfc3566_vectors() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let vectors = [
            (0, "75f0251d528ac01c4573dfd584d79f29"),
            (3, "5b376580ae2f19afe7219ceef172756f"),
            (16, "d2a246fa349b68a79998a4394ff7a263"),
            (20, "47f51b4564966215b8985c63055ed308"),
            (32, "f54f0ec8d2b9f3d36807734bd5283fd4"),
            (34, "becbb3bccdb518a30677d5481fb6b4d8"),
        ];
        for (len, want) in vectors {
            let msg = seq(len);
            assert_eq!(hex(&xcbc(&key, &[&msg])), want, "n={len}");
            let split = seq(len);
            let mut pieces: Vec<&[u8]> = Vec::new();
            for chunk in split.chunks(5) {
                pieces.push(chunk);
            }
            assert_eq!(hex(&xcbc(&key, &pieces)), want, "split n={len}");
        }
    }

    #[test]
    fn generate_sak_sek_vector() {
        let ks_host = seq(32);
        let (sak, sek) = generate_sak_sek(&ks_host.try_into().unwrap());
        assert_eq!(hex(&sek), "3fadf125b4b4663fa66e0598a5a6fcb1");
        assert_eq!(hex(&sak), "1596d2560c954632b66571ff425423b9");
    }

    #[test]
    fn key_from_precursor_vector() {
        let kp: Vec<u8> = (0x20..0x40).collect();
        let (key, iv) = key_from_precursor(&kp.try_into().unwrap());
        assert_eq!(
            hex(&[key.as_slice(), iv.as_slice()].concat()),
            "f6b56368b3d69e585295a7d39a0a973efd4e5eeeb7193d75f4549bc4ff70ef0b"
        );
    }

    #[test]
    fn sac_crypt_vector() {
        let key: [u8; 16] = std::array::from_fn(|i| 0x40 + i as u8);
        let plain = seq(48);
        let mut data = plain.clone();
        sac_crypt(&key, &mut data, true);
        assert_eq!(
            hex(&data),
            "e82d592f21ccad79e6dcf6a052cefe7d1d142845cf8a3c9aa4eb0bb2bcdb42e6\
             e4d49bd8efd6b4c4fef184917d659df4"
        );
        sac_crypt(&key, &mut data, false);
        assert_eq!(data, plain);
    }

    #[test]
    fn uri_confirm_vector() {
        let sak: [u8; 16] = seq(16).try_into().unwrap();
        let msg = [0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b];
        assert_eq!(
            hex(&uri_confirm(&msg, &sak)),
            "f33820970e3bd26ad25ccb402b50ed65854f26371bdf3526abbaa0bc46ebb1d2"
        );
    }

    #[test]
    fn sac_auth_round_trip() {
        let sak: [u8; 16] = seq(16).try_into().unwrap();
        let body = b"sac payload";
        let mac = sac_gen_auth(&sak, body);
        let mut data = body.to_vec();
        data.extend_from_slice(&mac);
        assert!(sac_check_auth(&sak, &data));

        let mut bad = data.clone();
        bad[0] ^= 0x01;
        assert!(!sac_check_auth(&sak, &bad));
        assert!(!sac_check_auth(&sak, &data[..15]));
    }

    #[test]
    fn dh_mod_exp_small() {
        assert_eq!(
            dh_mod_exp(&[4], &[13], &[0x01, 0xf1], 4).unwrap(),
            vec![0x00, 0x00, 0x01, 0xbd]
        );
        assert!(dh_mod_exp(&[4], &[13], &[0x01, 0xf1], 1).is_none());
    }

    #[test]
    fn dhph_signature_layout_and_verification() {
        let rsa = openssl::rsa::Rsa::generate(2048).unwrap();
        let key = PKey::from_rsa(rsa).unwrap();
        let nonce = [0x5au8; 32];
        let dhph = [0xa5u8; 256];
        let signature = dhph_signature(&nonce, &dhph, &key).unwrap();

        let mut msg = [0u8; 0x12e];
        msg[0x00] = 0x00;
        msg[0x01] = 0x00;
        msg[0x02] = 0x08;
        msg[0x03] = 0x01;
        msg[0x04] = 0x01;
        msg[0x05] = 0x00;
        msg[0x06] = 0x08;
        msg[0x07] = 0x02;
        msg[0x08] = 0x02;
        msg[0x09] = 0x01;
        msg[0x0a] = 0x00;
        msg[0x0b..0x2b].copy_from_slice(&nonce);
        msg[0x2b] = 0x04;
        msg[0x2c] = 0x08;
        msg[0x2d] = 0x00;
        msg[0x2e..].copy_from_slice(&dhph);

        assert_eq!(&msg[0x00..0x0b], &[0, 0, 8, 1, 1, 0, 8, 2, 2, 1, 0]);
        assert_eq!(&msg[0x2b..0x2e], &[4, 8, 0]);

        let mut verifier = openssl::sign::Verifier::new(MessageDigest::sha1(), &key).unwrap();
        verifier
            .set_rsa_padding(openssl::rsa::Padding::PKCS1_PSS)
            .unwrap();
        verifier
            .set_rsa_pss_saltlen(RsaPssSaltlen::custom(20))
            .unwrap();
        verifier.set_rsa_mgf1_md(MessageDigest::sha1()).unwrap();
        verifier.update(&msg).unwrap();
        assert!(verifier.verify(&signature).unwrap());
    }

    #[test]
    fn add_padding_cases() {
        let mut aligned = vec![1u8; 16];
        add_padding(&mut aligned, 0, 16);
        assert_eq!(aligned.len(), 16);

        let mut one_short = vec![1u8; 15];
        add_padding(&mut one_short, 0, 16);
        assert_eq!(&one_short[15..], &[0x80]);

        let mut five_short = vec![1u8; 11];
        add_padding(&mut five_short, 0, 16);
        assert_eq!(&five_short[11..], &[0x80, 0, 0, 0, 0]);

        let mut prefixed = vec![9u8; 4];
        add_padding(&mut prefixed, 4, 16);
        assert_eq!(prefixed.len(), 4, "nothing to pad after the prefix");
    }

    #[test]
    fn hex_decoding() {
        assert_eq!(
            str2bin(b"0123456789ABCDEF"),
            vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
        assert_eq!(str2bin(b"xy"), vec![0x32]);
        assert_eq!(str2bin(b"zz"), vec![0x33]);
        assert_eq!(str2bin(b"??"), vec![0]);
    }

    #[test]
    fn dh_gen_exp_fills_buffer() {
        struct Fixed;
        impl Rng for Fixed {
            fn fill(&mut self, buf: &mut [u8]) -> bool {
                buf.fill(0x5a);
                true
            }
        }
        assert_eq!(dh_gen_exp(&mut Fixed), [0x5a; 256]);
    }
}
