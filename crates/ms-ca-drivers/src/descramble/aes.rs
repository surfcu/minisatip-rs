//! Small AES-128 block cipher (FIPS-197) for the ECB/CBC descramblers.
//! Replaces the OpenSSL `EVP_aes_128_*` calls of `aes.cpp`.

/// AES block size in bytes.
pub const BLOCK: usize = 16;

const SBOX: [u8; 256] = sbox();
const INV_SBOX: [u8; 256] = inv_sbox();

const fn xtime(x: u8) -> u8 {
    (x << 1) ^ (((x >> 7) & 1) * 0x1B)
}

const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

const fn sbox() -> [u8; 256] {
    let mut s = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // Multiplicative inverse (x^254) followed by the affine transform.
        let mut inv = 1u8;
        let mut k = 0;
        while k < 254 {
            inv = gmul(inv, i as u8);
            k += 1;
        }
        let inv = if i == 0 { 0 } else { inv };
        s[i] = inv
            ^ inv.rotate_left(1)
            ^ inv.rotate_left(2)
            ^ inv.rotate_left(3)
            ^ inv.rotate_left(4)
            ^ 0x63;
        i += 1;
    }
    s
}

const fn inv_sbox() -> [u8; 256] {
    let s = sbox();
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[s[i] as usize] = i as u8;
        i += 1;
    }
    inv
}

/// AES-128 with an expanded key schedule.
#[derive(Clone)]
pub struct Aes128 {
    round_keys: [[u8; BLOCK]; 11],
}

impl Aes128 {
    /// Expands `key`.
    pub fn new(key: &[u8; BLOCK]) -> Self {
        let mut w = [[0u8; 4]; 44];
        for (i, word) in w.iter_mut().take(4).enumerate() {
            word.copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [
                    SBOX[t[1] as usize],
                    SBOX[t[2] as usize],
                    SBOX[t[3] as usize],
                    SBOX[t[0] as usize],
                ];
                t[0] ^= rcon;
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut round_keys = [[0u8; BLOCK]; 11];
        for (r, rk) in round_keys.iter_mut().enumerate() {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Self { round_keys }
    }

    fn add_round_key(state: &mut [u8; BLOCK], rk: &[u8; BLOCK]) {
        for (s, k) in state.iter_mut().zip(rk) {
            *s ^= k;
        }
    }

    /// Encrypts one block in place.
    pub fn encrypt_block(&self, block: &mut [u8; BLOCK]) {
        Self::add_round_key(block, &self.round_keys[0]);
        for round in 1..11 {
            for b in block.iter_mut() {
                *b = SBOX[*b as usize];
            }
            shift_rows(block);
            if round != 10 {
                for c in 0..4 {
                    let col = [
                        block[4 * c],
                        block[4 * c + 1],
                        block[4 * c + 2],
                        block[4 * c + 3],
                    ];
                    let all = col[0] ^ col[1] ^ col[2] ^ col[3];
                    for r in 0..4 {
                        block[4 * c + r] = col[r] ^ all ^ xtime(col[r] ^ col[(r + 1) % 4]);
                    }
                }
            }
            Self::add_round_key(block, &self.round_keys[round]);
        }
    }

    /// Decrypts one block in place.
    pub fn decrypt_block(&self, block: &mut [u8; BLOCK]) {
        Self::add_round_key(block, &self.round_keys[10]);
        for round in (0..10).rev() {
            inv_shift_rows(block);
            for b in block.iter_mut() {
                *b = INV_SBOX[*b as usize];
            }
            Self::add_round_key(block, &self.round_keys[round]);
            if round != 0 {
                for c in 0..4 {
                    let [a0, a1, a2, a3] = [
                        block[4 * c],
                        block[4 * c + 1],
                        block[4 * c + 2],
                        block[4 * c + 3],
                    ];
                    block[4 * c] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
                    block[4 * c + 1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
                    block[4 * c + 2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
                    block[4 * c + 3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
                }
            }
        }
    }

    /// ECB-decrypts every whole block of `data`; a trailing partial block is kept.
    pub fn decrypt_ecb(&self, data: &mut [u8]) {
        for chunk in data.as_chunks_mut::<BLOCK>().0 {
            self.decrypt_block(chunk);
        }
    }

    /// ECB-encrypts every whole block of `data`; a trailing partial block is kept.
    pub fn encrypt_ecb(&self, data: &mut [u8]) {
        for chunk in data.as_chunks_mut::<BLOCK>().0 {
            self.encrypt_block(chunk);
        }
    }

    /// CBC-decrypts every whole block of `data` from `iv`; a trailing partial block is kept.
    pub fn decrypt_cbc(&self, iv: &[u8; BLOCK], data: &mut [u8]) {
        let mut prev = *iv;
        for chunk in data.as_chunks_mut::<BLOCK>().0 {
            let cipher = *chunk;
            self.decrypt_block(chunk);
            for (b, p) in chunk.iter_mut().zip(&prev) {
                *b ^= p;
            }
            prev = cipher;
        }
    }

    /// CBC-encrypts every whole block of `data` from `iv`; a trailing partial block is kept.
    pub fn encrypt_cbc(&self, iv: &[u8; BLOCK], data: &mut [u8]) {
        let mut prev = *iv;
        for chunk in data.as_chunks_mut::<BLOCK>().0 {
            for (b, p) in chunk.iter_mut().zip(&prev) {
                *b ^= p;
            }
            self.encrypt_block(chunk);
            prev = *chunk;
        }
    }
}

fn shift_rows(s: &mut [u8; BLOCK]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            s[4 * c + r] = t[4 * ((c + r) % 4) + r];
        }
    }
}

fn inv_shift_rows(s: &mut [u8; BLOCK]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            s[4 * ((c + r) % 4) + r] = t[4 * c + r];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn key(s: &str) -> [u8; BLOCK] {
        hex(s).try_into().unwrap()
    }

    const SP800_KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
    const SP800_PT: &str = "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51";

    #[test]
    fn sbox_spot_values() {
        assert_eq!(SBOX[0x00], 0x63);
        assert_eq!(SBOX[0x53], 0xED);
        assert_eq!(INV_SBOX[0x63], 0x00);
    }

    // FIPS-197 appendix C.1.
    #[test]
    fn fips197_block() {
        let aes = Aes128::new(&key("000102030405060708090a0b0c0d0e0f"));
        let mut block: [u8; BLOCK] = key("00112233445566778899aabbccddeeff");
        aes.encrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
        aes.decrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex("00112233445566778899aabbccddeeff"));
    }

    // NIST SP 800-38A F.1.1 / F.1.2 (ECB-AES128), first two blocks.
    #[test]
    fn sp800_38a_ecb() {
        let aes = Aes128::new(&key(SP800_KEY));
        let mut data = hex(SP800_PT);
        aes.encrypt_ecb(&mut data);
        assert_eq!(
            data,
            hex("3ad77bb40d7a3660a89ecaf32466ef97f5d3d58503b9699de785895a96fdbaaf")
        );
        aes.decrypt_ecb(&mut data);
        assert_eq!(data, hex(SP800_PT));
    }

    // NIST SP 800-38A F.2.1 / F.2.2 (CBC-AES128), first two blocks.
    #[test]
    fn sp800_38a_cbc() {
        let aes = Aes128::new(&key(SP800_KEY));
        let iv = key("000102030405060708090a0b0c0d0e0f");
        let mut data = hex(SP800_PT);
        aes.encrypt_cbc(&iv, &mut data);
        assert_eq!(
            data,
            hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2")
        );
        aes.decrypt_cbc(&iv, &mut data);
        assert_eq!(data, hex(SP800_PT));
    }

    #[test]
    fn partial_block_is_untouched() {
        let aes = Aes128::new(&key(SP800_KEY));
        let mut data = vec![0xAB; 20];
        aes.decrypt_ecb(&mut data);
        assert_eq!(&data[16..], &[0xAB; 4]);
        aes.decrypt_cbc(&[0; BLOCK], &mut data);
        assert_eq!(&data[16..], &[0xAB; 4]);
    }
}
