//! Decrypt tests of `tests/test_pmt.cpp` ported, plus pipeline and end-to-end tests.

use std::sync::mpsc;

use ms_ca::{CaEvent, CaService, CwAlgo, StreamPid};
use ms_config::DropEncrypted;
use ms_ts::TsStage;

use super::aes::Aes128;
use super::*;
use crate::dvbapi::{DvbapiClient, DvbapiConfig, proto};

const NOW: i64 = 1_000;

// Ported from tests/test_pmt.cpp:71
#[cfg_attr(not(feature = "dvbcsa"), allow(dead_code))]
const PACKET: [u8; 188] = [
    0x47, 0x40, 0xff, 0x99, 0x14, 0x4c, 0x83, 0x7f, 0x46, 0xba, 0xb8, 0x12, 0xfb, 0x83, 0xf7, 0x50,
    0x9c, 0x73, 0x55, 0xe1, 0x8a, 0x1a, 0x54, 0x66, 0x87, 0xb1, 0xd6, 0x04, 0x10, 0xc4, 0xa9, 0xb8,
    0x53, 0x4e, 0x75, 0x11, 0xcd, 0xaf, 0xd7, 0x05, 0x9c, 0xea, 0x08, 0x65, 0x3b, 0x36, 0x62, 0xac,
    0xb2, 0x2c, 0xd3, 0x42, 0xb8, 0xfd, 0x67, 0x4d, 0xbf, 0xa3, 0x04, 0x4d, 0x0c, 0x0b, 0xb6, 0x70,
    0x3f, 0xaf, 0xcc, 0x26, 0x8c, 0xf2, 0x92, 0x7d, 0x64, 0x37, 0x18, 0x48, 0x0b, 0xd5, 0xd6, 0x50,
    0x2c, 0x79, 0xc5, 0xd9, 0x30, 0xb9, 0xb5, 0x9f, 0xca, 0x12, 0x0a, 0x10, 0xf2, 0x36, 0xa2, 0x23,
    0x3c, 0xc9, 0xb7, 0x70, 0x08, 0xfb, 0x94, 0x1d, 0x36, 0x79, 0x04, 0x5e, 0xe6, 0x70, 0xfa, 0xaf,
    0xe4, 0x12, 0x51, 0xad, 0x53, 0xb1, 0x48, 0xb7, 0x25, 0x67, 0x3c, 0xf5, 0x6f, 0x47, 0xe2, 0x97,
    0xe4, 0x93, 0xcb, 0x87, 0x4f, 0x77, 0x49, 0x7a, 0x7b, 0x7e, 0x26, 0xe0, 0xc9, 0xb4, 0x6e, 0x6a,
    0x52, 0xb8, 0xab, 0x25, 0xbf, 0x33, 0xb9, 0x4b, 0x25, 0x39, 0x26, 0x24, 0xaa, 0xa6, 0x19, 0xe1,
    0x3f, 0xbd, 0x33, 0x7f, 0xd9, 0xa5, 0xb4, 0x25, 0x44, 0xb1, 0x45, 0xee, 0xee, 0x25, 0x04, 0x47,
    0xcd, 0x63, 0x81, 0x03, 0x15, 0x59, 0x58, 0x1d, 0x00, 0x00, 0x00, 0x00,
];
// Ported from tests/test_pmt.cpp:88
const CW0: [u8; 8] = [0x64, 0xBB, 0x0E, 0x2D, 0x98, 0xAD, 0x8C, 0xD1];
// Ported from tests/test_pmt.cpp:89
#[cfg_attr(not(feature = "dvbcsa"), allow(dead_code))]
const CW1: [u8; 8] = [0x77, 0xC1, 0x1F, 0x57, 0x96, 0xFB, 0xC3, 0x54];
// Ported from tests/test_pmt.cpp:90
#[cfg_attr(not(feature = "dvbcsa"), allow(dead_code))]
const CW_INVALID: [u8; 8] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];

/// Stands in for a registered CSA op where no decrypt happens.
struct NullCipher;

impl Cipher for NullCipher {
    fn set_cw(&mut self, _key: &KeyMaterial) {}
    fn decrypt(&mut self, _key: &KeyMaterial, _payloads: &mut [&mut [u8]]) {}
}

fn null_csa_registry() -> CipherRegistry {
    let mut registry = CipherRegistry::empty();
    registry.register(CwAlgo::Csa, std::sync::Arc::new(|| Box::new(NullCipher)));
    registry
}

/// A clear PES packet; only PUSI packets start with `00 00 01`.
fn pes_packet(pid: u16, cc: u8, pusi: bool, seed: u8) -> [u8; 188] {
    let mut b = [0u8; 188];
    b[0] = 0x47;
    b[1] = if pusi { 0x40 } else { 0 } | (pid >> 8) as u8;
    b[2] = pid as u8;
    b[3] = 0x10 | (cc & 0x0F);
    for (i, x) in b[4..].iter_mut().enumerate() {
        *x = (seed.wrapping_add(i as u8)).wrapping_mul(31) ^ 0x5A;
    }
    if pusi {
        b[4..8].copy_from_slice(&[0, 0, 1, 0xE0]);
    } else {
        b[4] = 0xAA;
    }
    b
}

fn aes_key(cw: [u8; 8]) -> [u8; 16] {
    let mut k = [0; 16];
    k[..8].copy_from_slice(&cw);
    k
}

fn scramble_aes(b: &mut [u8; 188], cw: [u8; 8], parity: u8) {
    let start = adaptation_len(b);
    Aes128::new(&aes_key(cw)).encrypt_ecb(&mut b[start..]);
    b[3] |= 0x80 | (parity << 6);
}

fn scrambled_aes(
    pid: u16,
    pusi: bool,
    seed: u8,
    cw: [u8; 8],
    parity: u8,
) -> ([u8; 188], [u8; 188]) {
    let clear = pes_packet(pid, seed, pusi, seed);
    let mut s = clear;
    scramble_aes(&mut s, cw, parity);
    (clear, s)
}

fn aes_descrambler(pmt_id: usize, pids: &[u16]) -> Descrambler {
    let mut d = Descrambler::new(CipherRegistry::software());
    d.add_service(pmt_id, pids);
    d
}

fn send(d: &mut Descrambler, pmt: usize, algo: CwAlgo, parity: u8, cw: &[u8], now: i64) -> usize {
    d.send_cw(pmt, algo, parity, cw, None, 25, 0, now).unwrap()
}

fn assert_clear(buf: &[u8], expected: &[[u8; 188]]) {
    for (i, (got, want)) in buf.chunks(188).zip(expected).enumerate() {
        assert_eq!(got, want, "packet {i}");
    }
}

// Ported from tests/test_pmt.cpp:134
#[cfg(feature = "dvbcsa")]
#[test]
fn test_decrypt() {
    let max_len = 1000;
    let mut d = Descrambler::new(CipherRegistry::software());
    d.add_service(0, &[0xff]);
    let mut buf = PACKET.repeat(max_len);
    let ecm = 0;
    assert_eq!(
        d.send_cw(0, CwAlgo::Csa, 0, &CW_INVALID, None, 25, ecm, NOW),
        Ok(0)
    );
    assert_eq!(
        d.send_cw(0, CwAlgo::Csa, 0, &CW0, None, 25, ecm, NOW),
        Ok(1)
    );
    assert_eq!(
        d.send_cw(0, CwAlgo::Csa, 1, &CW1, None, 25, ecm, NOW),
        Ok(2)
    );
    assert_eq!(
        d.send_cw(0, CwAlgo::Csa, 0, &CW_INVALID, None, 25, ecm, NOW),
        Err(SendCwError::Duplicate)
    );

    assert!(
        !d.test_slot(0, &[PACKET]),
        "test_decrypt_packet expected to fail"
    );
    assert!(
        d.test_slot(1, &[PACKET]),
        "test_decrypt_packet expected to work"
    );

    // Without a packet start the CW cannot be verified: no decrypt stamp
    for i in 0..max_len {
        buf[i * 188 + 1] &= !0x40;
    }
    d.set_stamps(0, -1, -1);
    d.decrypt_stream(&mut buf, NOW);
    let stats = d.stats(0).unwrap();
    assert!(stats.last_scrambled_pkt >= 0, "scrambled packets stamped");
    assert_eq!(stats.last_decrypt_ok, -1, "unverified CW not stamped");
    buf.copy_from_slice(&PACKET.repeat(max_len));

    d.decrypt_stream(&mut buf, NOW);
    let b = &buf[(max_len - 1) * 188..];
    assert_eq!(
        b[4] as u32 + b[5] as u32 + b[6] as u32,
        1,
        "MPEG header expected"
    );
    let stats = d.stats(0).unwrap();
    assert!(
        stats.last_decrypt_ok >= stats.last_scrambled_pkt,
        "a verified decrypt is stamped"
    );
    assert_eq!(b[3] & 0xC0, 0, "scrambling bits cleared");
}

// Ported from tests/test_pmt.cpp:1935
#[test]
fn test_cw_keyed_by_pmt() {
    let mut d = Descrambler::new(null_csa_registry());
    let id = 0;
    d.add_service(id, &[]);
    let cw = [0x10, 0x22, 0x34, 0x66, 0x88, 0x9A, 0xAC, 0xC6];
    let slot = d.send_cw(id, CwAlgo::Csa, 0, &cw, None, 25, 0, NOW);
    assert!(slot.is_ok(), "send_cw should store the CW");
    let found = (0..MAX_CW).filter(|&i| d.slot(i).is_some_and(|s| s.3 && s.0 == id));
    assert_eq!(found.count(), 1, "CW should be keyed by the PMT id");
    assert_eq!(
        d.send_cw(id, CwAlgo::Csa, 0, &cw, None, 25, 0, NOW),
        Err(SendCwError::Duplicate),
        "duplicate CW should be rejected"
    );
}

// Ported from tests/test_pmt.cpp:1972
#[test]
fn test_send_cw_skips_in_use_slot() {
    let mut d = Descrambler::new(null_csa_registry());
    let id = 0;
    d.add_service(id, &[]);
    let cw_a = [0x10, 0x22, 0x34, 0x66, 0x88, 0x9A, 0xAC, 0xC6];
    let cw_b = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let slot = d
        .send_cw(id, CwAlgo::Csa, 0, &cw_a, None, 25, 0, NOW)
        .unwrap();

    d.set_active_cw(id, Some(slot)); // adapter thread picked it up (update_cw)
    d.set_slot_expiry(slot, NOW - 1); // ... then it expired
    let other = d.send_cw(id, CwAlgo::Csa, 0, &cw_b, None, 25, 0, NOW);
    assert!(other.is_ok(), "send_cw should store the second CW");
    assert_eq!(
        d.slot(slot).unwrap().2,
        cw_a,
        "in-use expired slot must not be reused"
    );
    let other = other.unwrap();
    assert_ne!(other, slot);
    let (pmt, _, key, enabled) = d.slot(other).unwrap();
    assert!(
        enabled && pmt == id && key == cw_b,
        "second CW should land in another slot"
    );
}

#[test]
fn send_cw_errors_match_cpp_return_codes() {
    let mut d = Descrambler::new(CipherRegistry::software());
    assert_eq!(
        d.send_cw(7, CwAlgo::Aes128Ecb, 0, &CW0, None, 0, 0, NOW),
        Err(SendCwError::PmtNotFound)
    );
    d.add_service(7, &[0x100]);
    // No software DES in C++ either (only a hardware descrambler registers it).
    assert_eq!(
        d.send_cw(7, CwAlgo::Des, 0, &CW0, None, 0, 0, NOW),
        Err(SendCwError::NoAlgo)
    );
    #[cfg(not(feature = "dvbcsa"))]
    assert_eq!(
        d.send_cw(7, CwAlgo::Csa, 0, &CW0, None, 0, 0, NOW),
        Err(SendCwError::NoAlgo)
    );
    #[cfg(not(feature = "dvbcsa-icam"))]
    assert_eq!(
        d.send_cw(7, CwAlgo::CsaIcam, 0, &CW0, None, 0, 0, NOW),
        Err(SendCwError::NoAlgo)
    );
}

#[test]
fn cw_slots_fill_up_and_expired_slots_are_reused() {
    let mut d = Descrambler::new(null_csa_registry());
    d.add_service(1, &[]);
    for i in 0..MAX_CW {
        let cw = (i as u64).to_be_bytes();
        assert_eq!(d.send_cw(1, CwAlgo::Csa, 0, &cw, None, 1, 0, NOW), Ok(i));
    }
    let cw = u64::MAX.to_be_bytes();
    assert_eq!(
        d.send_cw(1, CwAlgo::Csa, 0, &cw, None, 1, 0, NOW),
        Err(SendCwError::Full)
    );
    // Expiry 1 s; zero means MAX_CW_TIME.
    assert_eq!(
        d.send_cw(1, CwAlgo::Csa, 0, &cw, None, 0, 0, NOW + 1001),
        Ok(0)
    );
    assert_eq!(d.slot_expiry(0), Some(NOW + 1001 + MAX_CW_TIME));
}

#[test]
fn slots_of_another_algorithm_are_not_reused() {
    let mut registry = null_csa_registry();
    registry.register(
        CwAlgo::Aes128Ecb,
        std::sync::Arc::new(|| Box::new(AesEcb::default())),
    );
    let mut d = Descrambler::new(registry);
    d.add_service(1, &[]);
    assert_eq!(send(&mut d, 1, CwAlgo::Csa, 0, &CW0, NOW), 0);
    d.set_slot_expiry(0, 0);
    assert_eq!(send(&mut d, 1, CwAlgo::Aes128Ecb, 0, &CW0, NOW), 1);
    assert_eq!(send(&mut d, 1, CwAlgo::Csa, 0, &[0x99; 8], NOW), 0);
}

#[test]
fn parity_switch_flushes_the_batch_and_selects_the_next_key() {
    let even = [1, 2, 3, 4, 5, 6, 7, 8];
    let odd = [8, 7, 6, 5, 4, 3, 2, 1];
    let mut d = aes_descrambler(3, &[0x101]);
    send(&mut d, 3, CwAlgo::Aes128Ecb, 0, &even, NOW);
    send(&mut d, 3, CwAlgo::Aes128Ecb, 1, &odd, NOW);

    let mut clear = Vec::new();
    let mut buf = Vec::new();
    for i in 0..8u8 {
        let (parity, cw) = if i < 4 { (0, even) } else { (1, odd) };
        let (c, s) = scrambled_aes(0x101, i % 4 == 0, i, cw, parity);
        clear.push(c);
        buf.extend_from_slice(&s);
    }
    d.decrypt_stream(&mut buf, NOW + 5);
    assert_clear(&buf, &clear);
    let stats = d.stats(3).unwrap();
    assert_eq!(stats.parity, 1);
    assert_eq!(stats.cw, Some(1));
    assert_eq!(stats.last_decrypt_ok, NOW + 5);
    assert_eq!(stats.last_scrambled_pkt, NOW + 5);

    // The next buffer keeps the odd key without searching again.
    let (c, s) = scrambled_aes(0x101, false, 9, odd, 1);
    let mut buf = s.to_vec();
    d.decrypt_stream(&mut buf, NOW + 10);
    assert_clear(&buf, &[c]);
    assert_eq!(d.stats(3).unwrap().last_decrypt_ok, NOW + 10);
}

#[test]
fn parity_change_without_pusi_takes_the_newest_cw_unverified() {
    let old = [1; 8];
    let new = [2; 8];
    let mut d = aes_descrambler(0, &[0x101]);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 1, &old, NOW);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 1, &new, NOW + 100);
    let (c, s) = scrambled_aes(0x101, false, 1, new, 1);
    let mut buf = s.to_vec();
    d.decrypt_stream(&mut buf, NOW + 200);
    assert_clear(&buf, &[c]);
    let stats = d.stats(0).unwrap();
    assert_eq!(stats.cw, Some(1));
    assert_eq!(stats.last_decrypt_ok, 0, "unverified CW not stamped");

    // A packet start then verifies the key and stamps the decrypt.
    let (c, s) = scrambled_aes(0x101, true, 2, new, 1);
    let mut buf = s.to_vec();
    d.decrypt_stream(&mut buf, NOW + 300);
    assert_clear(&buf, &[c]);
    assert_eq!(d.stats(0).unwrap().last_decrypt_ok, NOW + 300);
}

#[test]
fn invalid_cw_leaves_packets_scrambled_and_drop_nulls_them() {
    let mut d = aes_descrambler(0, &[0x101]);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &[9; 8], NOW);
    let (_, s) = scrambled_aes(0x101, true, 1, [3; 8], 0);
    let (_, s2) = scrambled_aes(0x101, false, 2, [3; 8], 0);
    let mut buf = [s, s2].concat();
    d.decrypt_stream(&mut buf, NOW);
    assert_eq!(buf, [s, s2].concat(), "no CW validates: nothing decrypted");
    assert_eq!(d.stats(0).unwrap().cw, None);

    d.mark_pids_null(&mut buf, DropEncrypted::Drop, false);
    for b in buf.chunks(188) {
        assert_eq!(((b[1] as u16 & 0x1F) << 8) | b[2] as u16, 0x1FFF);
    }
    assert_eq!(d.dec_err(), 2);

    // The right key arriving later is found on the next start packet.
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &[3; 8], NOW + 1);
    let mut buf = [s, s2].concat();
    d.decrypt_stream(&mut buf, NOW + 2);
    assert_eq!(buf[3] & 0xC0, 0);
    assert_eq!(&buf[4..7], &[0, 0, 1]);
}

#[test]
fn validated_cw_shortens_older_keys_of_the_same_parity() {
    let a = [0xA; 8];
    let b = [0xB; 8];
    let odd = [0x0D; 8];
    let mut d = aes_descrambler(0, &[0x101]);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &a, NOW);
    let mut buf = scrambled_aes(0x101, true, 1, a, 0).1.to_vec();
    d.decrypt_stream(&mut buf, NOW);
    assert_eq!(d.stats(0).unwrap().cw, Some(0));

    send(&mut d, 0, CwAlgo::Aes128Ecb, 1, &odd, NOW + 1);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &b, NOW + 2);
    let mut buf = [
        scrambled_aes(0x101, true, 2, odd, 1).1,
        scrambled_aes(0x101, true, 3, b, 0).1,
    ]
    .concat();
    d.decrypt_stream(&mut buf, NOW + 3);
    assert_eq!(d.stats(0).unwrap().cw, Some(2));
    assert_eq!(d.slot_expiry(0), Some(NOW + 3 + 10_000));
    assert_eq!(d.slot_expiry(2), Some(NOW + 2 + 25_000));
}

#[test]
fn new_cw_for_the_active_parity_ranks_below_the_active_one() {
    let good = [0x11; 8];
    let fake = [0x22; 8];
    let odd = [0x33; 8];
    let mut d = aes_descrambler(0, &[0x101]);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &good, NOW);
    let mut buf = scrambled_aes(0x101, true, 1, good, 0).1.to_vec();
    d.decrypt_stream(&mut buf, NOW + 10);
    // Verified and active: a second CW for the same parity may be fake.
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &fake, NOW + 20);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 1, &odd, NOW + 20);

    // Two parity changes without PUSI: the newest even CW must still be `good`.
    let (c1, s1) = scrambled_aes(0x101, false, 2, odd, 1);
    let (c2, s2) = scrambled_aes(0x101, false, 3, good, 0);
    let mut buf = [s1, s2].concat();
    d.decrypt_stream(&mut buf, NOW + 30);
    assert_clear(&buf, &[c1, c2]);
    assert_eq!(d.stats(0).unwrap().cw, Some(0));
}

#[test]
fn interleaved_services_batch_independently() {
    let k1 = [0x41; 8];
    let k2e = [0x42; 8];
    let k2o = [0x43; 8];
    let mut d = Descrambler::new(CipherRegistry::software());
    d.add_service(1, &[0x101, 0x102]);
    d.add_service(2, &[0x201]);
    send(&mut d, 1, CwAlgo::Aes128Ecb, 0, &k1, NOW);
    send(&mut d, 2, CwAlgo::Aes128Ecb, 0, &k2e, NOW);
    send(&mut d, 2, CwAlgo::Aes128Ecb, 1, &k2o, NOW);

    let mut clear = Vec::new();
    let mut buf = Vec::new();
    let mut push = |c: [u8; 188], s: [u8; 188]| {
        clear.push(c);
        buf.extend_from_slice(&s);
    };
    let (c, s) = scrambled_aes(0x101, true, 0, k1, 0);
    push(c, s);
    let (c, s) = scrambled_aes(0x201, true, 1, k2e, 0);
    push(c, s);
    let (c, s) = scrambled_aes(0x102, false, 2, k1, 0);
    push(c, s);
    let (c, s) = scrambled_aes(0x201, true, 3, k2o, 1);
    push(c, s);
    push(pes_packet(0x300, 4, true, 4), pes_packet(0x300, 4, true, 4));
    let (c, s) = scrambled_aes(0x101, false, 5, k1, 0);
    push(c, s);
    d.decrypt_stream(&mut buf, NOW);
    assert_clear(&buf, &clear);
    assert_eq!(d.stats(1).unwrap().parity, 0);
    assert_eq!(d.stats(2).unwrap().parity, 1);
}

#[test]
fn adaptation_field_payload_is_decrypted_after_the_field() {
    let cw = [0x5C; 8];
    let mut clear = pes_packet(0x101, 0, true, 0);
    clear[3] = 0x30;
    clear[4] = 7;
    clear[5] = 0x00;
    clear[6..12].fill(0xFF);
    clear[12..16].copy_from_slice(&[0, 0, 1, 0xC0]);
    let mut s = clear;
    scramble_aes(&mut s, cw, 0);
    assert_eq!(&s[4..12], &clear[4..12], "adaptation field stays clear");
    let mut d = aes_descrambler(0, &[0x101]);
    send(&mut d, 0, CwAlgo::Aes128Ecb, 0, &cw, NOW);
    let mut buf = s.to_vec();
    d.decrypt_stream(&mut buf, NOW);
    assert_clear(&buf, &[clear]);
    assert_eq!(d.stats(0).unwrap().last_decrypt_ok, NOW);
}

#[test]
fn aes_cbc_restarts_from_the_iv_on_every_packet() {
    let cw = [0xC8; 8];
    let iv = [0x10; 16];
    let mut d = aes_descrambler(0, &[0x101]);
    d.send_cw(0, CwAlgo::Aes128Cbc, 1, &cw, Some(&iv[..]), 0, 0, NOW)
        .unwrap();
    let aes = Aes128::new(&aes_key(cw));
    let mut clear = Vec::new();
    let mut buf = Vec::new();
    for i in 0..3u8 {
        let c = pes_packet(0x101, i, i == 0, i);
        let mut s = c;
        aes.encrypt_cbc(&iv, &mut s[4..]);
        s[3] |= 0xC0;
        clear.push(c);
        buf.extend_from_slice(&s);
    }
    d.decrypt_stream(&mut buf, NOW);
    assert_clear(&buf, &clear);
}

#[test]
fn removing_a_service_drops_its_cws_and_pids() {
    let cw = [0x77; 8];
    let mut d = aes_descrambler(4, &[0x101]);
    send(&mut d, 4, CwAlgo::Aes128Ecb, 0, &cw, NOW);
    send(&mut d, 4, CwAlgo::Aes128Ecb, 1, &[0x78; 8], NOW);
    d.remove_service(4);
    assert!(!d.has_service(4));
    assert!(!d.slot(0).unwrap().3 && !d.slot(1).unwrap().3);
    let (_, s) = scrambled_aes(0x101, true, 0, cw, 0);
    let mut buf = s.to_vec();
    d.decrypt_stream(&mut buf, NOW);
    assert_eq!(buf, s);
    assert_eq!(
        d.send_cw(4, CwAlgo::Aes128Ecb, 0, &cw, None, 0, 0, NOW),
        Err(SendCwError::PmtNotFound)
    );
}

#[test]
fn drop_modes_rewrite_scrambled_packets_like_mark_pids_null() {
    let mut d = Descrambler::new(CipherRegistry::software());
    let mut scrambled = pes_packet(0x101, 0, true, 0);
    scrambled[3] |= 0x80;
    let mut with_pcr = scrambled;
    with_pcr[3] = 0xB0;
    with_pcr[4] = 7;
    with_pcr[5] = 0x10 | 0x40;

    let mut b = scrambled;
    d.mark_pids_null(&mut b, DropEncrypted::Drop, false);
    assert_eq!((b[1], b[2]), (0x5F, 0xFF));

    let mut b = with_pcr;
    d.mark_pids_null(&mut b, DropEncrypted::Drop, false);
    assert_eq!(&b[..6], &[0x47, 0x01, 0x01, 0x20, 183, 0x90]);
    assert_eq!(&b[6..12], &with_pcr[6..12], "PCR kept");
    assert!(b[12..].iter().all(|&x| x == 0xFF));

    let mut b = scrambled;
    d.mark_pids_null(&mut b, DropEncrypted::Stuff, false);
    assert_eq!(&b[..6], &[0x47, 0x01, 0x01, 0x20, 183, 0x00]);
    assert!(b[6..].iter().all(|&x| x == 0xFF));

    let mut b = scrambled;
    d.mark_pids_null(&mut b, DropEncrypted::Send, false);
    assert_eq!(b, scrambled);
    d.mark_pids_null(&mut b, DropEncrypted::Drop, true);
    assert_eq!(b, scrambled, "pids=all with -o ~ keeps scrambled packets");
    assert_eq!(d.dec_err(), 4);
}

fn fixed_clock(now: i64) -> Box<dyn FnMut() -> i64 + Send> {
    Box::new(move || now)
}

#[test]
fn stage_passes_clear_adapters_through_untouched() {
    let (tx, mut stage) = DescramblerStage::with_clock(
        CipherRegistry::software(),
        StageConfig::default(),
        fixed_clock(NOW),
    );
    tx.send(DescramblerCommand::AddService {
        pmt_id: 0,
        pids: vec![0x101],
    })
    .unwrap();
    let (_, s) = scrambled_aes(0x101, true, 0, [1; 8], 0);
    let clear = pes_packet(0x200, 0, true, 9);
    let mut buf = [s, clear].concat();
    stage.process(&mut buf);
    assert_eq!(buf, [s, clear].concat(), "no CA on the adapter: untouched");

    tx.send(DescramblerCommand::SetCaActive(true)).unwrap();
    let mut buf = clear.to_vec();
    stage.process(&mut buf);
    assert_eq!(buf, clear, "clear-text packets untouched");
}

#[test]
fn stage_query_reports_stamps() {
    let (tx, mut stage) = DescramblerStage::with_clock(
        CipherRegistry::software(),
        StageConfig::default(),
        fixed_clock(NOW),
    );
    tx.send(DescramblerCommand::SetCaActive(true)).unwrap();
    tx.send(DescramblerCommand::AddService {
        pmt_id: 2,
        pids: vec![0x101],
    })
    .unwrap();
    let mut buf = scrambled_aes(0x101, true, 0, [1; 8], 0).1.to_vec();
    stage.process(&mut buf);
    let (reply, rx) = mpsc::channel();
    tx.send(DescramblerCommand::Query(reply)).unwrap();
    stage.drain_commands();
    let stats = rx.recv().unwrap();
    assert_eq!(stats.dec_err, 1);
    assert_eq!(stats.services[0].pmt_id, 2);
    assert_eq!(stats.services[0].last_scrambled_pkt, NOW);
    assert_eq!(stats.services[0].last_decrypt_ok, 0);
}

/// Server bytes a fake oscam sends: SERVER_INFO, DESCR_MODE, CA_SET_DESCR.
fn server_info() -> Vec<u8> {
    let info = b"OSCam v1.20, build r11720 (x86_64-linux)";
    let mut b = proto::DVBAPI_SERVER_INFO.to_be_bytes().to_vec();
    b.extend_from_slice(&2u16.to_be_bytes());
    b.push(info.len() as u8);
    b.extend_from_slice(info);
    b
}

fn ca_set_descr_mode(key: u8, algo: i32, mode: i32) -> Vec<u8> {
    let mut b = proto::DVBAPI_CA_SET_DESCR_MODE.to_be_bytes().to_vec();
    b.push(key);
    b.extend_from_slice(&0i32.to_be_bytes());
    b.extend_from_slice(&algo.to_be_bytes());
    b.extend_from_slice(&mode.to_be_bytes());
    b
}

fn ca_set_descr(key: u8, parity: i32, cw: [u8; 8]) -> Vec<u8> {
    let mut b = proto::DVBAPI_CA_SET_DESCR.to_be_bytes().to_vec();
    b.push(key);
    b.extend_from_slice(&0i32.to_be_bytes());
    b.extend_from_slice(&parity.to_be_bytes());
    b.extend_from_slice(&cw);
    b
}

/// A registered DVBAPI client serving PMT 0 (video PID 0x101) on adapter 0.
fn oscam_client() -> DvbapiClient {
    let cfg = DvbapiConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
        ..DvbapiConfig::default()
    };
    let mut client = DvbapiClient::new(cfg);
    client.on_connected(NOW);
    client.on_data(&server_info(), NOW);
    assert_eq!(client.next_event(), Some(CaEvent::Ready));
    let service = CaService {
        pmt_id: 0,
        sid: 100,
        pmt_pid: 48,
        streams: vec![StreamPid {
            stream_type: 2,
            pid: 0x101,
        }],
        ..CaService::default()
    };
    client.add_service(&service, false, NOW).unwrap();
    while client.take_output().is_some() {}
    client
}

/// Runs oscam bytes through the client and forwards its events to the stage.
fn deliver(client: &mut DvbapiClient, tx: &mpsc::Sender<DescramblerCommand>, bytes: &[u8]) {
    client.on_data(bytes, NOW);
    while let Some(event) = client.next_event() {
        tx.send(DescramblerCommand::Ca(event)).unwrap();
    }
}

fn e2e_stage() -> (mpsc::Sender<DescramblerCommand>, DescramblerStage) {
    let (tx, stage) = DescramblerStage::with_clock(
        CipherRegistry::software(),
        StageConfig::default(),
        fixed_clock(NOW),
    );
    tx.send(DescramblerCommand::SetCaActive(true)).unwrap();
    tx.send(DescramblerCommand::AddService {
        pmt_id: 0,
        pids: vec![0x101],
    })
    .unwrap();
    (tx, stage)
}

#[test]
fn end_to_end_fake_oscam_aes_ecb() {
    let mut client = oscam_client();
    let (tx, mut stage) = e2e_stage();
    let cw = [0x21, 0x43, 0x65, 0x87, 0xA9, 0xCB, 0xED, 0x0F];
    deliver(
        &mut client,
        &tx,
        &ca_set_descr_mode(0, proto::CW_ALGO_AES128, proto::CW_ALGO_MODE_ECB),
    );
    deliver(&mut client, &tx, &ca_set_descr(0, 1, cw));

    let mut clear = Vec::new();
    let mut buf = Vec::new();
    for i in 0..20u8 {
        let (c, s) = scrambled_aes(0x101, i % 10 == 0, i, cw, 1);
        clear.push(c);
        buf.extend_from_slice(&s);
    }
    let fta = pes_packet(0x200, 0, true, 50);
    clear.push(fta);
    buf.extend_from_slice(&fta);
    let unknown = scrambled_aes(0x300, true, 51, cw, 1).1;
    buf.extend_from_slice(&unknown);

    stage.process(&mut buf);
    assert_clear(&buf, &clear);
    let last = &buf[21 * 188..];
    assert_eq!(((last[1] as u16 & 0x1F) << 8) | last[2] as u16, 0x1FFF);
    let stats = stage.descrambler().stats(0).unwrap();
    assert_eq!(stats.last_decrypt_ok, NOW);
    assert_eq!(stage.descrambler().dec_err(), 1);
}

#[cfg(feature = "dvbcsa")]
#[test]
fn end_to_end_fake_oscam_csa() {
    use super::csa::Csa;
    let mut client = oscam_client();
    let (tx, mut stage) = e2e_stage();
    let even = CW0;
    let odd = [0x13, 0x57, 0x9B, 0x05, 0x24, 0x68, 0xAC, 0x38];
    deliver(&mut client, &tx, &ca_set_descr(0, 0, even));
    deliver(&mut client, &tx, &ca_set_descr(0, 1, odd));

    let mut clear = Vec::new();
    let mut buf = Vec::new();
    for i in 0..300u16 {
        let parity = (i >= 150) as u8;
        let c = pes_packet(0x101, i as u8, i % 50 == 0, i as u8);
        let mut s = c;
        let mut csa = Csa::new(false);
        let key = KeyMaterial {
            cw: {
                let mut k = [0; 32];
                k[..8].copy_from_slice(if parity == 0 { &even } else { &odd });
                k
            },
            iv: [0; 32],
            cw_len: 8,
            icam_ecm: 0,
        };
        csa.set_cw(&key);
        csa.encrypt(&mut [&mut s[4..]]);
        s[3] |= 0x80 | (parity << 6);
        clear.push(c);
        buf.extend_from_slice(&s);
    }
    stage.process(&mut buf);
    assert_clear(&buf, &clear);
    let stats = stage.descrambler().stats(0).unwrap();
    assert_eq!((stats.parity, stats.last_decrypt_ok), (1, NOW));
}

#[cfg(feature = "dvbcsa")]
#[test]
fn csa_test_vector_decrypts_with_cw0_only() {
    let mut d = Descrambler::new(CipherRegistry::software());
    d.add_service(0, &[0xff]);
    send(&mut d, 0, CwAlgo::Csa, 0, &CW1, NOW);
    let mut buf = PACKET.to_vec();
    d.decrypt_stream(&mut buf, NOW);
    assert_eq!(buf, PACKET, "wrong parity key never applies");
    send(&mut d, 0, CwAlgo::Csa, 0, &CW0, NOW);
    d.decrypt_stream(&mut buf, NOW);
    assert_eq!(&buf[4..7], &[0, 0, 1]);
    assert_eq!(buf[3], 0x19);
}

#[test]
fn updating_service_pids_keeps_the_control_words() {
    let mut d = Descrambler::new(null_csa_registry());
    d.add_service(1, &[0x100]);
    assert_eq!(send(&mut d, 1, CwAlgo::Csa, 0, &CW0, NOW), 0);
    d.set_service_pids(1, &[0x100, 0x101]);
    assert!(
        d.slot(0)
            .is_some_and(|(pmt, _, _, enabled)| pmt == 1 && enabled)
    );
    // add_service is the reset path: it drops the CWs.
    d.add_service(1, &[0x100]);
    assert!(d.slot(0).is_some_and(|(_, _, _, enabled)| !enabled));
    // An unknown service is added.
    d.set_service_pids(2, &[0x200]);
    assert!(d.has_service(2));
}

/// A TS packet on `pid` with payload, scrambled (TSC 10) or clear.
fn ts(pid: u16, scrambled: bool) -> [u8; 188] {
    let mut b = [0xAAu8; 188];
    b[0] = 0x47;
    b[1] = (pid >> 8) as u8 & 0x1F;
    b[2] = pid as u8;
    b[3] = if scrambled { 0x90 } else { 0x10 };
    b
}

fn batch(packets: &[(u16, bool)]) -> Vec<u8> {
    packets.iter().flat_map(|&(pid, s)| ts(pid, s)).collect()
}

/// count_packets then mark_pids_null, as one data-plane batch with a CA active.
fn pass(d: &mut Descrambler, packets: &[(u16, bool)]) {
    let mut buf = batch(packets);
    d.count_packets(&buf, false);
    d.mark_pids_null(&mut buf, DropEncrypted::Send, false);
}

// get_adapter_decerrs (adapter.cpp:2363): the errors of the two busiest active
// pids; mark_pids_null (pmt.cpp:1324-1374) counts per pid and resets on decrypt.
#[test]
fn ad_decerr_sums_the_two_busiest_active_pids() {
    let mut d = Descrambler::new(CipherRegistry::software());
    d.set_adapter_pids(&[
        (0x100, true, 1),
        (0x101, true, 1),
        (0x102, true, 1),
        (0x103, false, 1),
    ]);
    let mut packets = vec![(0x100, true); 10];
    packets.extend([(0x101, true); 6]);
    packets.extend([(0x102, true); 2]);
    packets.extend([(0x103, true); 20]);
    pass(&mut d, &packets);
    assert_eq!(d.dec_err(), 38, "ad->dec_err counts every scrambled packet");
    assert_eq!(d.pid_counters(0x102).dec_err, 2);
    assert_eq!(
        d.adapter_decerr(),
        16,
        "0x100 and 0x101; 0x103 is not active yet"
    );

    // The first clear packet of a pid resets its errors, once.
    pass(&mut d, &[(0x100, false)]);
    assert_eq!(d.pid_counters(0x100).dec_err, 0);
    assert!(d.pid_counters(0x100).decrypted);
    pass(&mut d, &[(0x100, true), (0x100, false)]);
    assert_eq!(d.pid_counters(0x100).dec_err, 1, "no second reset");
    assert_eq!(d.adapter_decerr(), 7);
}

#[test]
fn a_claim_change_rearms_the_reset_and_removed_pids_are_forgotten() {
    let mut d = Descrambler::new(CipherRegistry::software());
    d.set_adapter_pids(&[(0x100, true, 1)]);
    pass(&mut d, &[(0x100, false), (0x100, true), (0x100, true)]);
    assert_eq!(d.pid_counters(0x100).dec_err, 2);
    // pmt.cpp:1264: a PMT claiming the pid clears is_decrypted.
    d.set_adapter_pids(&[(0x100, true, 2)]);
    assert!(!d.pid_counters(0x100).decrypted);
    pass(&mut d, &[(0x100, false)]);
    assert_eq!(
        d.pid_counters(0x100).dec_err,
        0,
        "reset again after the claim change"
    );

    d.set_adapter_pids(&[]);
    assert_eq!(d.pid_counters(0x100), PidCounters::default());
    pass(&mut d, &[(0x100, true)]);
    assert_eq!(
        d.pid_counters(0x100).dec_err,
        0,
        "untracked pids have no counters"
    );
    assert_eq!(d.adapter_decerr(), 0);
}

#[test]
fn no_packets_are_counted_while_pid_8192_is_requested() {
    let mut d = Descrambler::new(CipherRegistry::software());
    d.set_adapter_pids(&[(0x100, true, 1)]);
    d.count_packets(&batch(&[(0x100, true); 5]), true);
    assert_eq!(d.pid_counters(0x100).packets, 0);
    d.count_packets(&batch(&[(0x100, true); 5]), false);
    assert_eq!(d.pid_counters(0x100).packets, 5);
}

// The stage counts scrambled packets left after decrypting and reports them
// as ad_decerr; the first clear packet clears them.
#[test]
fn stage_reports_ad_decerr_and_clears_it_once_decrypted() {
    let (tx, mut stage) = DescramblerStage::new(
        CipherRegistry::software(),
        StageConfig {
            drop_encrypted: DropEncrypted::Send,
            pids_all_no_dec: false,
        },
        std::time::Instant::now(),
    );
    tx.send(DescramblerCommand::SetCaActive(true)).unwrap();
    tx.send(DescramblerCommand::SetAdapterPids(vec![(0x100, true, 1)]))
        .unwrap();
    let query = |tx: &mpsc::Sender<DescramblerCommand>, stage: &mut DescramblerStage| {
        let (reply, rx) = mpsc::channel();
        tx.send(DescramblerCommand::Query(reply)).unwrap();
        stage.drain_commands();
        rx.recv().unwrap().ad_decerr
    };
    stage.process(&mut batch(&[(0x100, true); 4]));
    assert_eq!(query(&tx, &mut stage), 4);
    stage.process(&mut batch(&[(0x100, false)]));
    assert_eq!(query(&tx, &mut stage), 0);
}

// adapter.cpp:1015-1028: a pid turning NEW -> ACTIVE starts its counters at zero.
#[test]
fn a_new_pid_becoming_active_starts_from_zero() {
    let mut d = Descrambler::new(CipherRegistry::software());
    d.set_adapter_pids(&[(0x100, false, 1)]);
    pass(&mut d, &[(0x100, true), (0x100, true)]);
    assert_eq!(d.pid_counters(0x100).packets, 2);
    assert_eq!(d.adapter_decerr(), 0, "a NEW pid is not summed");
    d.set_adapter_pids(&[(0x100, true, 1)]);
    assert_eq!(
        (d.pid_counters(0x100).packets, d.pid_counters(0x100).dec_err),
        (0, 0)
    );
    pass(&mut d, &[(0x100, true)]);
    assert_eq!(d.adapter_decerr(), 1);
}

// send_cw_to_all_pmts: a CI+ key arrives with its AES-CBC IV.
#[test]
fn cw_with_iv_decrypts_aes_cbc_and_is_redacted() {
    let key = [0xAB; 16];
    let iv = [0xCD; 16];
    let (tx, mut stage) = DescramblerStage::with_clock(
        CipherRegistry::software(),
        StageConfig::default(),
        fixed_clock(NOW),
    );
    for command in [
        DescramblerCommand::AddService {
            pmt_id: 0,
            pids: vec![0x101],
        },
        DescramblerCommand::SetCaActive(true),
        DescramblerCommand::CwWithIv {
            pmt_id: 0,
            algo: CwAlgo::Aes128Cbc,
            parity: 0,
            cw: key.to_vec(),
            iv,
            expiry_s: 3720,
            redact: true,
        },
    ] {
        tx.send(command).unwrap();
    }

    let clear = pes_packet(0x101, 0, true, 0);
    let mut scrambled = clear;
    Aes128::new(&key).encrypt_cbc(&iv, &mut scrambled[4..]);
    scrambled[3] |= 0x80;
    let mut buf = scrambled.to_vec();
    stage.process(&mut buf);
    assert_eq!(buf, clear, "AES-128-CBC from the CI+ IV");
    assert_eq!(
        stage.descrambler().stats(0).unwrap().last_decrypt_ok,
        NOW,
        "the CW validated"
    );

    let line = stage
        .descrambler()
        .cw_dump_line(0, NOW)
        .expect("the slot is live");
    assert!(line.contains("CW: <redacted>"), "{line}");
    assert!(line.contains("IV: <redacted>"), "{line}");
    assert!(!line.contains("AB"), "no key bytes: {line}");
    assert!(!line.contains("CD"), "no IV bytes: {line}");
}
