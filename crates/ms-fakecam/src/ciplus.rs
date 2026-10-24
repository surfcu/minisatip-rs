//! CI+ content control as seen from the CAM (module side of ca.cpp's host).
//!
//! An independent implementation: no `ms-ca-drivers` dependency, its own
//! AES/XCBC/SHA/DH and OpenSSL certificate and RSA-PSS verification.

use openssl::bn::{BigNum, BigNumContext};
use openssl::hash::{Hasher, MessageDigest};
use openssl::nid::Nid;
use openssl::rand::rand_bytes;
use openssl::rsa::Padding;
use openssl::sign::{RsaPssSaltlen, Verifier};
use openssl::stack::Stack;
use openssl::symm::{Cipher, Crypter, Mode};
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::verify::X509VerifyFlags;
use openssl::x509::{X509, X509StoreContext};
use std::fmt;

use crate::tags::*;

pub mod test_pki;

/// `dh_p` (ca.cpp:109).
const DH_P: [u8; 256] = [
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

/// `dh_g` (ca.cpp:134).
const DH_G: [u8; 256] = [
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

/// CI+ content control configuration of one fake CAM.
#[derive(Clone, PartialEq, Eq)]
pub struct CiPlusCam {
    /// PEM of the root the host chain must verify against.
    pub root_pem: Vec<u8>,
    /// DER of the CAM brand certificate (element 8).
    pub brand_cert_der: Vec<u8>,
    /// DER of the CAM device certificate (element 16).
    pub device_cert_der: Vec<u8>,
    /// PEM of the CAM device key; never printed.
    pub device_key_pem: Vec<u8>,
    /// Order of the step-B elements; the default is `[14, 16, 18, 8]`.
    pub element_order: [u8; 4],
    /// Key register announced in element 28.
    pub parity: u8,
    /// Run the optional URI confirmation step after the key exchange.
    pub uri_step: bool,
    /// Request only element 22 first, to check the cached AKH after a reconnect.
    pub cached_akh_probe: bool,
    /// Request only element 29, to check the URI version bitmask.
    pub probe_uri_mask: bool,
}

impl Default for CiPlusCam {
    fn default() -> Self {
        Self {
            root_pem: Vec::new(),
            brand_cert_der: Vec::new(),
            device_cert_der: Vec::new(),
            device_key_pem: Vec::new(),
            element_order: [14, 16, 18, 8],
            parity: 0,
            uri_step: false,
            cached_akh_probe: false,
            probe_uri_mask: false,
        }
    }
}

impl fmt::Debug for CiPlusCam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CiPlusCam")
            .field("root_pem", &self.root_pem.len())
            .field("brand_cert_der", &self.brand_cert_der.len())
            .field("device_cert_der", &self.device_cert_der.len())
            .field("device_key_pem", &"[redacted]")
            .field("element_order", &self.element_order)
            .field("parity", &self.parity)
            .field("uri_step", &self.uri_step)
            .field("cached_akh_probe", &self.cached_akh_probe)
            .field("probe_uri_mask", &self.probe_uri_mask)
            .finish()
    }
}

/// What the host side of the SAC handshake did, for the tests.
#[derive(Clone, PartialEq, Eq)]
pub struct CiPlusStatus {
    /// Steps that completed with the expected values.
    pub steps: u32,
    /// The AKH returned by the host matched the one the CAM computed.
    pub akh_match: bool,
    /// The URI confirmation matched.
    pub uri_match: bool,
    /// Element 29's bitmask matched the opened resource, when probed.
    pub uri_mask_match: Option<bool>,
    /// The SAC key reply decrypted and authenticated.
    pub key_ok: bool,
    /// Every SAC header echoed the serial, 0x0100 and the payload length.
    pub sac_header_ok: bool,
    /// Key the host derived from the last precursor.
    pub expected_key: Option<[u8; 16]>,
    pub expected_iv: Option<[u8; 16]>,
    pub expected_parity: Option<u8>,
}

impl fmt::Debug for CiPlusStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CiPlusStatus")
            .field("steps", &self.steps)
            .field("akh_match", &self.akh_match)
            .field("uri_match", &self.uri_match)
            .field("uri_mask_match", &self.uri_mask_match)
            .field("key_ok", &self.key_ok)
            .field("sac_header_ok", &self.sac_header_ok)
            .field("expected_key", &self.expected_key.map(|_| "[redacted]"))
            .field("expected_iv", &self.expected_iv.map(|_| "[redacted]"))
            .field("expected_parity", &self.expected_parity)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    AwaitOpenCnf,
    AwaitAuthReply,
    AwaitDhReply,
    AwaitAkhReply,
    AwaitNsReply,
    AwaitKeyReply,
    AwaitUriReply,
    AwaitMaskReply,
    AwaitProbeAkh,
    AwaitSacSyncReply,
    AwaitCcSyncReply,
    Done,
}

/// Per-slot CI+ state of the fake CAM; kept across CAM reconnects.
pub(crate) struct CcRuntime {
    cfg: CiPlusCam,
    resource: u32,
    stage: Stage,
    nonce: [u8; 32],
    dhph: Option<[u8; 256]>,
    host_id: Option<[u8; 8]>,
    dhsk: Option<[u8; 256]>,
    akh: Option<[u8; 32]>,
    ks_host: Option<[u8; 32]>,
    sak: [u8; 16],
    sek: [u8; 16],
    ns_module: [u8; 8],
    uri_msg: [u8; 8],
    serial: u32,
    steps: u32,
    akh_match: bool,
    uri_match: bool,
    uri_mask_match: Option<bool>,
    key_ok: bool,
    sac_header_ok: bool,
    expected: Option<([u8; 16], [u8; 16], u8)>,
}

impl CcRuntime {
    pub(crate) fn new(cfg: CiPlusCam) -> Self {
        Self {
            cfg,
            resource: CIPLUS_CC,
            stage: Stage::Idle,
            nonce: [0; 32],
            dhph: None,
            host_id: None,
            dhsk: None,
            akh: None,
            ks_host: None,
            sak: [0; 16],
            sek: [0; 16],
            ns_module: [0; 8],
            uri_msg: *b"CI+URI01",
            serial: 1,
            steps: 0,
            akh_match: false,
            uri_match: false,
            uri_mask_match: None,
            key_ok: false,
            sac_header_ok: true,
            expected: None,
        }
    }

    pub(crate) fn status(&self) -> CiPlusStatus {
        CiPlusStatus {
            steps: self.steps,
            akh_match: self.akh_match,
            uri_match: self.uri_match,
            uri_mask_match: self.uri_mask_match,
            key_ok: self.key_ok,
            sac_header_ok: self.sac_header_ok,
            expected_key: self.expected.map(|e| e.0),
            expected_iv: self.expected.map(|e| e.1),
            expected_parity: self.expected.map(|e| e.2),
        }
    }

    pub(crate) fn resource(&self) -> u32 {
        self.resource
    }

    /// `CIPLUS_APP_CC_create` on the CAM: open the handshake with OPEN_REQ.
    pub(crate) fn open(&mut self, resource: u32) -> Vec<(u32, Vec<u8>)> {
        self.resource = resource;
        self.stage = Stage::AwaitOpenCnf;
        self.steps = 0;
        self.akh_match = false;
        self.uri_match = false;
        self.uri_mask_match = None;
        self.key_ok = false;
        self.sac_header_ok = true;
        self.expected = None;
        vec![(TAG_CC_OPEN_REQ, Vec::new())]
    }

    /// One CC APDU from the host; returns the APDUs the CAM answers with.
    pub(crate) fn on_apdu(&mut self, tag: u32, body: &[u8]) -> Vec<(u32, Vec<u8>)> {
        match tag {
            TAG_CC_OPEN_CNF => self.on_open_cnf(),
            TAG_CC_DATA_CNF => self.on_data_cnf(body),
            TAG_CC_SAC_DATA_CNF => self.on_sac_data_cnf(body),
            TAG_CC_SAC_SYNC_CNF => self.on_sac_sync_cnf(body),
            TAG_CC_SYNC_CNF => self.on_cc_sync_cnf(body),
            _ => Vec::new(),
        }
    }

    fn on_open_cnf(&mut self) -> Vec<(u32, Vec<u8>)> {
        self.steps += 1;
        if self.cfg.probe_uri_mask {
            self.stage = Stage::AwaitMaskReply;
            return vec![(TAG_CC_DATA_REQ, build_data_req(&[], &[29]))];
        }
        if self.cfg.cached_akh_probe && self.akh.is_some() {
            self.stage = Stage::AwaitProbeAkh;
            return vec![(TAG_CC_DATA_REQ, build_data_req(&[], &[22]))];
        }
        rand_fill(&mut self.nonce);
        self.stage = Stage::AwaitAuthReply;
        let request = build_data_req(&[(19, &self.nonce)], &[7, 13, 15, 17]);
        vec![(TAG_CC_DATA_REQ, request)]
    }

    fn on_data_cnf(&mut self, body: &[u8]) -> Vec<(u32, Vec<u8>)> {
        let count = usize::from(body.get(1).copied().unwrap_or(0));
        let Some(elements) = parse_elements(body.get(2..).unwrap_or(&[]), count) else {
            return Vec::new();
        };
        match self.stage {
            Stage::AwaitAuthReply => self.finish_auth_reply(&elements),
            Stage::AwaitDhReply => {
                if element(&elements, 30) == Some(&[0u8][..]) {
                    self.steps += 1;
                }
                self.stage = Stage::AwaitAkhReply;
                vec![(TAG_CC_DATA_REQ, build_data_req(&[], &[22]))]
            }
            Stage::AwaitAkhReply => {
                let host_akh = element(&elements, 22).unwrap_or(&[]);
                self.akh_match = self.akh.as_ref().is_some_and(|akh| akh == host_akh);
                if self.akh_match {
                    self.steps += 1;
                }
                self.start_key_seed()
            }
            Stage::AwaitNsReply => self.finish_key_seed(&elements),
            Stage::AwaitMaskReply => {
                let mask = if self.resource == CIPLUS_CC { 1 } else { 3 };
                self.uri_mask_match = element(&elements, 29).map(|data| data.last() == Some(&mask));
                if self.uri_mask_match == Some(true) {
                    self.steps += 1;
                }
                self.stage = Stage::Done;
                Vec::new()
            }
            Stage::AwaitProbeAkh => {
                let host_akh = element(&elements, 22).unwrap_or(&[]);
                self.akh_match = self.akh.as_ref().is_some_and(|akh| akh == host_akh);
                if self.akh_match {
                    self.steps += 1;
                }
                self.stage = Stage::Done;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Step B1 reply: verify the host chain and Signature_A, send step B2.
    fn finish_auth_reply(&mut self, elements: &[(u8, Vec<u8>)]) -> Vec<(u32, Vec<u8>)> {
        let (Some(brand), Some(device), Some(signature)) = (
            element(elements, 7),
            element(elements, 15),
            element(elements, 17),
        ) else {
            return Vec::new();
        };
        let Some(dhph) = element(elements, 13).and_then(|data| <[u8; 256]>::try_from(data).ok())
        else {
            return Vec::new();
        };
        let Some(device) = self.verify_host_chain(brand, device) else {
            self.stage = Stage::Done;
            return Vec::new();
        };
        if !verify_signature(&device, &self.nonce, &dhph, signature) {
            self.stage = Stage::Done;
            return Vec::new();
        }
        let Some(host_id) = cert_hostid(&device) else {
            self.stage = Stage::Done;
            return Vec::new();
        };
        self.host_id = Some(host_id);
        self.dhph = Some(dhph);

        let mut exp = [0u8; 256];
        rand_fill(&mut exp);
        let Some(dhpm) = mod_exp(&DH_G, &exp) else {
            return Vec::new();
        };
        let Some(dhsk) = mod_exp(&dhph, &exp) else {
            return Vec::new();
        };
        let Some(cicam_cert) = X509::from_der(&self.cfg.device_cert_der).ok() else {
            return Vec::new();
        };
        let Some(cicam_id) = cert_hostid(&cicam_cert) else {
            return Vec::new();
        };
        let akh = sha256(&[&cicam_id, &host_id, &dhsk]);
        self.dhsk = Some(dhsk);
        self.akh = Some(akh);

        let mut signature_b = [0u8; 256];
        rand_fill(&mut signature_b);
        let mut sent: Vec<(u8, Vec<u8>)> = Vec::new();
        for &id in &self.cfg.element_order {
            let data = match id {
                14 => dhpm.to_vec(),
                16 => self.cfg.device_cert_der.clone(),
                18 => signature_b.to_vec(),
                8 => self.cfg.brand_cert_der.clone(),
                _ => continue,
            };
            sent.push((id, data));
        }
        let refs: Vec<(u8, &[u8])> = sent
            .iter()
            .map(|(id, data)| (*id, data.as_slice()))
            .collect();
        self.stage = Stage::AwaitDhReply;
        vec![(TAG_CC_DATA_REQ, build_data_req(&refs, &[30]))]
    }

    /// Step C: send Ns_module and request Ns_host.
    fn start_key_seed(&mut self) -> Vec<(u32, Vec<u8>)> {
        rand_fill(&mut self.ns_module);
        self.stage = Stage::AwaitNsReply;
        vec![(
            TAG_CC_DATA_REQ,
            build_data_req(&[(21, &self.ns_module)], &[20]),
        )]
    }

    /// Step C reply: derive ks_host/SAK/SEK, then send the key precursor.
    fn finish_key_seed(&mut self, elements: &[(u8, Vec<u8>)]) -> Vec<(u32, Vec<u8>)> {
        let (Some(dhsk), Some(akh)) = (self.dhsk.as_ref(), self.akh.as_ref()) else {
            return Vec::new();
        };
        let Some(ns_host) = element(elements, 20) else {
            return Vec::new();
        };
        let ks_host = sha256(&[&dhsk[240..], akh, ns_host, &self.ns_module]);
        let (sak, sek) = generate_sak_sek(&ks_host);
        self.ks_host = Some(ks_host);
        self.sak = sak;
        self.sek = sek;
        self.steps += 1;

        let mut precursor = [0u8; 32];
        rand_fill(&mut precursor);
        let (key, iv) = key_from_precursor(&precursor);
        self.expected = Some((key, iv, self.cfg.parity));
        let serial = self.next_serial();
        let parity = self.cfg.parity;
        let request = build_sac_data_req(
            &self.sek,
            &self.sak,
            serial,
            &[(12, &precursor), (28, &[parity])],
            &[30],
        );
        self.stage = Stage::AwaitKeyReply;
        vec![(TAG_CC_SAC_DATA_REQ, request)]
    }

    fn on_sac_data_cnf(&mut self, body: &[u8]) -> Vec<(u32, Vec<u8>)> {
        if !matches!(self.stage, Stage::AwaitKeyReply | Stage::AwaitUriReply) {
            return Vec::new();
        }
        let Some(plain) = open_sac(&self.sek, &self.sak, body) else {
            return Vec::new();
        };
        if !self.sac_header_matches(&plain) {
            return Vec::new();
        }
        let count = usize::from(plain.get(9).copied().unwrap_or(0));
        let Some(elements) = parse_elements(plain.get(10..).unwrap_or(&[]), count) else {
            return Vec::new();
        };
        match self.stage {
            Stage::AwaitKeyReply => {
                self.key_ok = element(&elements, 30) == Some(&[0u8][..]);
                if self.key_ok {
                    self.steps += 1;
                }
                if self.cfg.uri_step {
                    self.stage = Stage::AwaitUriReply;
                    let program = [0x00, 0x01];
                    let serial = self.next_serial();
                    let request = build_sac_data_req(
                        &self.sek,
                        &self.sak,
                        serial,
                        &[(25, &self.uri_msg), (26, &program)],
                        &[27],
                    );
                    vec![(TAG_CC_SAC_DATA_REQ, request)]
                } else {
                    self.send_sac_sync()
                }
            }
            Stage::AwaitUriReply => {
                let want = uri_confirm(&self.uri_msg, &self.sak);
                self.uri_match = element(&elements, 27) == Some(&want[..]);
                if self.uri_match {
                    self.steps += 1;
                }
                self.send_sac_sync()
            }
            _ => Vec::new(),
        }
    }

    /// The host echoes the request serial, 0x0100 and the padded payload length.
    fn sac_header_matches(&mut self, plain: &[u8]) -> bool {
        let serial = plain
            .get(..4)
            .and_then(|data| <[u8; 4]>::try_from(data).ok())
            .map(u32::from_be_bytes);
        let len = plain
            .get(6..8)
            .and_then(|data| <[u8; 2]>::try_from(data).ok())
            .map(u16::from_be_bytes);
        let ok = serial == Some(self.serial)
            && plain.get(4..6) == Some(&[0x01, 0x00])
            && len.map(usize::from) == plain.len().checked_sub(8 + 16);
        if !ok {
            self.sac_header_ok = false;
        }
        ok
    }

    fn on_sac_sync_cnf(&mut self, body: &[u8]) -> Vec<(u32, Vec<u8>)> {
        let Some(plain) = open_sac(&self.sek, &self.sak, body) else {
            return Vec::new();
        };
        if !self.sac_header_matches(&plain) {
            return Vec::new();
        }
        if plain.get(8) == Some(&0) {
            self.steps += 1;
        }
        self.stage = Stage::AwaitCcSyncReply;
        vec![(TAG_CC_SYNC_REQ, Vec::new())]
    }

    fn on_cc_sync_cnf(&mut self, body: &[u8]) -> Vec<(u32, Vec<u8>)> {
        if body == [0] {
            self.steps += 1;
        }
        self.stage = Stage::Done;
        Vec::new()
    }

    fn send_sac_sync(&mut self) -> Vec<(u32, Vec<u8>)> {
        let mut data = Vec::with_capacity(8);
        data.extend_from_slice(&self.next_serial().to_be_bytes());
        data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        self.stage = Stage::AwaitSacSyncReply;
        vec![(TAG_CC_SAC_SYNC_REQ, data)]
    }

    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Another key register request after the handshake (tests).
    pub(crate) fn key_request(&mut self, parity: u8) -> Option<Vec<u8>> {
        self.ks_host.as_ref()?;
        let mut precursor = [0u8; 32];
        rand_fill(&mut precursor);
        let (key, iv) = key_from_precursor(&precursor);
        self.expected = Some((key, iv, parity));
        let serial = self.next_serial();
        Some(build_sac_data_req(
            &self.sek,
            &self.sak,
            serial,
            &[(12, &precursor), (28, &[parity])],
            &[30],
        ))
    }

    fn verify_host_chain(&self, brand_der: &[u8], device_der: &[u8]) -> Option<X509> {
        let root = X509::from_pem(&self.cfg.root_pem).ok()?;
        let brand = X509::from_der(brand_der).ok()?;
        let device = X509::from_der(device_der).ok()?;
        let mut builder = X509StoreBuilder::new().ok()?;
        builder.set_flags(X509VerifyFlags::NO_CHECK_TIME).ok()?;
        builder.add_cert(root).ok()?;
        builder.add_cert(brand).ok()?;
        let store = builder.build();
        let chain = Stack::new().ok()?;
        let mut context = X509StoreContext::new().ok()?;
        let ok = context
            .init(&store, &device, &chain, |ctx| ctx.verify_cert())
            .ok()?;
        ok.then_some(device)
    }
}

fn element(elements: &[(u8, Vec<u8>)], id: u8) -> Option<&[u8]> {
    elements
        .iter()
        .find(|(element_id, _)| *element_id == id)
        .map(|(_, data)| data.as_slice())
}

fn parse_elements(mut data: &[u8], count: usize) -> Option<Vec<(u8, Vec<u8>)>> {
    let mut out = Vec::new();
    for _ in 0..count {
        let id = *data.first()?;
        let len = usize::from(u16::from_be_bytes([*data.get(1)?, *data.get(2)?]));
        data = &data[3..];
        let item = data.get(..len)?;
        out.push((id, item.to_vec()));
        data = &data[len..];
    }
    Some(out)
}

/// `send_CC_data_req`: `[id_bitmask, count, elements.., req_count, ids..]`.
fn build_data_req(elements: &[(u8, &[u8])], requests: &[u8]) -> Vec<u8> {
    let mut body = vec![0x00, elements.len() as u8];
    for (id, data) in elements {
        body.push(*id);
        body.extend_from_slice(&(data.len() as u16).to_be_bytes());
        body.extend_from_slice(data);
    }
    body.push(requests.len() as u8);
    body.extend_from_slice(requests);
    body
}

/// `send_SAC_data_req`: header, padded payload, XCBC MAC, AES-CBC.
fn build_sac_data_req(
    sek: &[u8; 16],
    sak: &[u8; 16],
    serial: u32,
    elements: &[(u8, &[u8])],
    requests: &[u8],
) -> Vec<u8> {
    let payload = build_data_req(elements, requests);
    let mut data = Vec::new();
    data.extend_from_slice(&serial.to_be_bytes());
    data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    data.extend_from_slice(&payload);
    add_padding(&mut data, 8, 16);
    let len = (data.len() - 8) as u16;
    data[6..8].copy_from_slice(&len.to_be_bytes());
    let mac = sac_gen_auth(sak, &data);
    data.extend_from_slice(&mac);
    aes_cbc(sek, &SAC_IV, &mut data[8..], true);
    data
}

/// Decrypts and authenticates one SAC APDU; `None` when the MAC is wrong.
fn open_sac(sek: &[u8; 16], sak: &[u8; 16], body: &[u8]) -> Option<Vec<u8>> {
    if body.len() < 16 || !(body.len() - 8).is_multiple_of(16) {
        return None;
    }
    let mut data = body.to_vec();
    aes_cbc(sek, &SAC_IV, &mut data[8..], false);
    let (signed, mac) = data.split_at(data.len() - 16);
    (sac_gen_auth(sak, signed) == mac).then_some(data)
}

fn rand_fill(buf: &mut [u8]) {
    rand_bytes(buf).expect("OpenSSL RNG");
}

fn mod_exp(base: &[u8], exp: &[u8]) -> Option<[u8; 256]> {
    let base = BigNum::from_slice(base).ok()?;
    let exp = BigNum::from_slice(exp).ok()?;
    let modulus = BigNum::from_slice(&DH_P).ok()?;
    let mut ctx = BigNumContext::new().ok()?;
    let mut result = BigNum::new().ok()?;
    result.mod_exp(&base, &exp, &modulus, &mut ctx).ok()?;
    let bytes = result.to_vec();
    if bytes.len() > 256 {
        return None;
    }
    let mut out = [0u8; 256];
    out[256 - bytes.len()..].copy_from_slice(&bytes);
    Some(out)
}

/// `dh_dhph_signature` message layout (ca.cpp:468-494).
fn signature_message(nonce: &[u8; 32], dhph: &[u8; 256]) -> [u8; 0x12e] {
    let mut msg = [0u8; 0x12e];
    msg[0x02] = 0x08;
    msg[0x03] = 0x01;
    msg[0x04] = 0x01;
    msg[0x06] = 0x08;
    msg[0x07] = 0x02;
    msg[0x08] = 0x02;
    msg[0x09] = 0x01;
    msg[0x0b..0x2b].copy_from_slice(nonce);
    msg[0x2b] = 0x04;
    msg[0x2c] = 0x08;
    msg[0x2e..].copy_from_slice(dhph);
    msg
}

fn verify_signature(cert: &X509, nonce: &[u8; 32], dhph: &[u8; 256], signature: &[u8]) -> bool {
    let Ok(key) = cert.public_key() else {
        return false;
    };
    let Ok(mut verifier) = Verifier::new(MessageDigest::sha1(), &key) else {
        return false;
    };
    let message = signature_message(nonce, dhph);
    verifier.set_rsa_padding(Padding::PKCS1_PSS).is_ok()
        && verifier
            .set_rsa_pss_saltlen(RsaPssSaltlen::custom(20))
            .is_ok()
        && verifier.set_rsa_mgf1_md(MessageDigest::sha1()).is_ok()
        && verifier.update(&message).is_ok()
        && verifier.verify(signature).unwrap_or(false)
}

/// The 16-character hex CN as 8 bytes, like `element_set_hostid_from_certificate`.
fn cert_hostid(cert: &X509) -> Option<[u8; 8]> {
    let data = cert
        .subject_name()
        .entries_by_nid(Nid::COMMONNAME)
        .next()?
        .data()
        .as_slice();
    if data.len() < 16 {
        return None;
    }
    let mut id = [0u8; 8];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = (nibble(data[2 * i])? << 4) | nibble(data[2 * i + 1])?;
    }
    Some(id)
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn aes_ecb(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let Ok(mut crypter) = Crypter::new(Cipher::aes_128_ecb(), Mode::Encrypt, key, None) else {
        return [0; 16];
    };
    crypter.pad(false);
    let mut out = [0u8; 32];
    let Ok(first) = crypter.update(block, &mut out) else {
        return [0; 16];
    };
    let Ok(last) = crypter.finalize(&mut out[first..]) else {
        return [0; 16];
    };
    if first + last != 16 {
        return [0; 16];
    }
    out[..16].try_into().unwrap_or([0; 16])
}

fn aes_cbc(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8], encrypt: bool) {
    let len = data.len() & !15;
    if len == 0 {
        return;
    }
    let mode = if encrypt {
        Mode::Encrypt
    } else {
        Mode::Decrypt
    };
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
}

/// `sac_gen_auth`: AES-XCBC-MAC over `0x04` and the whole APDU.
fn sac_gen_auth(sak: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let mut k = [[0u8; 16]; 3];
    for (i, block) in k.iter_mut().enumerate() {
        *block = [i as u8 + 1; 16];
        *block = aes_ecb(sak, block);
    }
    let mut state = [0u8; 16];
    let mut buffered = 0;
    for &byte in [&[0x04u8][..], data].concat().iter() {
        if buffered == 16 {
            state = aes_ecb(&k[0], &state);
            buffered = 0;
        }
        state[buffered] ^= byte;
        buffered += 1;
    }
    if buffered == 16 {
        for i in 0..16 {
            state[i] ^= k[1][i];
        }
    } else {
        state[buffered] ^= 0x80;
        for i in 0..16 {
            state[i] ^= k[2][i];
        }
    }
    aes_ecb(&k[0], &state)
}

fn add_padding(buf: &mut Vec<u8>, from: usize, block: usize) {
    if block == 0 || (buf.len().saturating_sub(from)).is_multiple_of(block) {
        return;
    }
    buf.push(0x80);
    while !(buf.len().saturating_sub(from)).is_multiple_of(block) {
        buf.push(0);
    }
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
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
        out.copy_from_slice(&digest[..32]);
    }
    out
}

/// `generate_SAK_SEK`: `(sak, sek)` from the 32-byte key seed.
fn generate_sak_sek(ks_host: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut first = [0u8; 16];
    let mut second = [0u8; 16];
    first.copy_from_slice(&ks_host[..16]);
    second.copy_from_slice(&ks_host[16..]);
    let dec_first = aes_ecb(&SAK_SEK_KEY, &first);
    let dec_second = aes_ecb(&SAK_SEK_KEY, &second);
    let mut sek = [0u8; 16];
    let mut sak = [0u8; 16];
    for i in 0..16 {
        sek[i] = ks_host[i] ^ dec_first[i];
        sak[i] = ks_host[16 + i] ^ dec_second[i];
    }
    (sak, sek)
}

/// The AES/XOR part of the host's `check_new_key`.
fn key_from_precursor(precursor: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut first = [0u8; 16];
    let mut second = [0u8; 16];
    first.copy_from_slice(&precursor[..16]);
    second.copy_from_slice(&precursor[16..]);
    let dec_first = aes_ecb(&PRECURSOR_KEY, &first);
    let dec_second = aes_ecb(&PRECURSOR_KEY, &second);
    let mut key = [0u8; 16];
    let mut iv = [0u8; 16];
    for i in 0..16 {
        key[i] = dec_first[i] ^ precursor[i];
        iv[i] = dec_second[i] ^ precursor[16 + i];
    }
    (key, iv)
}

/// `generate_uri_confirm`: `sha256(uri_msg || sha256(sak))`.
fn uri_confirm(uri_msg: &[u8], sak: &[u8; 16]) -> [u8; 32] {
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

    fn xcbc(key: &[u8; 16], msg: &[u8]) -> [u8; 16] {
        let mut k = [[0u8; 16]; 3];
        for (i, block) in k.iter_mut().enumerate() {
            *block = [i as u8 + 1; 16];
            *block = aes_ecb(key, block);
        }
        let mut state = [0u8; 16];
        let mut n = 0;
        for &b in msg {
            if n == 16 {
                state = aes_ecb(&k[0], &state);
                n = 0;
            }
            state[n] ^= b;
            n += 1;
        }
        if n == 16 {
            for i in 0..16 {
                state[i] ^= k[1][i];
            }
        } else {
            state[n] ^= 0x80;
            for i in 0..16 {
                state[i] ^= k[2][i];
            }
        }
        aes_ecb(&k[0], &state)
    }

    #[test]
    fn xcbc_mac_matches_host_vectors() {
        let key: [u8; 16] = seq(16).try_into().unwrap();
        let vectors = [
            (0, "75f0251d528ac01c4573dfd584d79f29"),
            (16, "d2a246fa349b68a79998a4394ff7a263"),
            (34, "becbb3bccdb518a30677d5481fb6b4d8"),
        ];
        for (len, want) in vectors {
            assert_eq!(hex(&xcbc(&key, &seq(len))), want, "n={len}");
        }
        assert_eq!(
            hex(&sac_gen_auth(&key, &[0x5a])),
            hex(&xcbc(&key, &[0x04, 0x5a]))
        );
    }

    #[test]
    fn sak_sek_and_precursor_match_host_vectors() {
        let ks_host: [u8; 32] = seq(32).try_into().unwrap();
        let (sak, sek) = generate_sak_sek(&ks_host);
        assert_eq!(hex(&sek), "3fadf125b4b4663fa66e0598a5a6fcb1");
        assert_eq!(hex(&sak), "1596d2560c954632b66571ff425423b9");

        let precursor: Vec<u8> = (0x20..0x40).collect();
        let (key, iv) = key_from_precursor(&precursor.try_into().unwrap());
        assert_eq!(
            hex(&[key.as_slice(), iv.as_slice()].concat()),
            "f6b56368b3d69e585295a7d39a0a973efd4e5eeeb7193d75f4549bc4ff70ef0b"
        );
    }

    #[test]
    fn sac_round_trip_and_uri_confirm() {
        let sek: [u8; 16] = seq(16).try_into().unwrap();
        let sak: [u8; 16] = (16..32)
            .map(|i| i as u8)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let body = build_sac_data_req(&sek, &sak, 7, &[(19, &[0x5a; 32])], &[30]);
        let plain = open_sac(&sek, &sak, &body).expect("MAC and cipher round trip");
        assert_eq!(&plain[..4], &7u32.to_be_bytes());
        assert_eq!(plain[8], 0);
        assert_eq!(plain[9], 1);
        let mut broken = body.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0x01;
        assert!(open_sac(&sek, &sak, &broken).is_none());

        let uri = [0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b];
        let sak: [u8; 16] = seq(16).try_into().unwrap();
        assert_eq!(
            hex(&uri_confirm(&uri, &sak)),
            "f33820970e3bd26ad25ccb402b50ed65854f26371bdf3526abbaa0bc46ebb1d2"
        );
    }
}
