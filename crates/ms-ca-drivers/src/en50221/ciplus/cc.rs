//! Host CI+ content control (`ci_ccmgr_*` and `CIPLUS_APP_CC_*`, ca.cpp:1729-2506).

use std::fmt;
use std::path::Path;

use openssl::pkey::{PKey, Private};

use super::authdata::{Record, get_authdata, write_authdata};
use super::certs::{CertCtx, cert_der, hostid_from_cert, private_key_from_pem};
use super::crypto::{
    DH_G, DH_P, OsRng, Rng, add_padding, dh_mod_exp, dhph_signature, generate_sak_sek,
    key_from_precursor, sac_check_auth, sac_crypt, sac_gen_auth, sha256, uri_confirm,
};
use super::elements::Elements;
use super::ffi::wipe;
use crate::en50221::consts::*;
use crate::en50221::registry::authdata_filename;

/// `#define DEFAULT_LOG LOG_DVBCA` (ca.cpp:52).
const DEFAULT_LOG: u32 = ms_log::LOG_DVBCA;

/// `struct cc_ctrl_data` (ca.h:168-196) plus the host configuration it reads.
pub struct CcState {
    elements: Elements,
    dhsk: [u8; 256],
    ks_host: [u8; 32],
    sek: [u8; 16],
    sak: [u8; 16],
    akh_index: u32,
    dh_exp: [u8; 256],
    cert_ctx: Option<CertCtx>,
    rsa_device_key: Option<PKey<Private>>,
    cert_dir: String,
    cache_dir: String,
    slot_id: u32,
    ci_name: Vec<u8>,
    uri_mask: i32,
    rng: Box<dyn Rng>,
}

/// What the device has to do after `CcState::handle`.
#[derive(Clone, PartialEq, Eq)]
pub enum CcAction {
    /// `ca_write_apdu`.
    Apdu { tag: u32, body: Vec<u8> },
    /// `check_new_key`: a descrambling key for `parity`.
    NewKey {
        parity: u8,
        key: [u8; 16],
        iv: [u8; 16],
    },
    /// `disable_cws_for_all_pmts` (ca.cpp:2348).
    DisableCws,
    /// `d->force_ci = 1` after a failed `restart_dh_challenge`.
    ForceCi,
}

impl fmt::Debug for CcAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Apdu { tag, body } => f
                .debug_struct("Apdu")
                .field("tag", &format_args!("{tag:06X}"))
                .field("body_len", &body.len())
                .finish(),
            Self::NewKey { parity, .. } => f
                .debug_struct("NewKey")
                .field("parity", parity)
                .field("key", &"[redacted]")
                .field("iv", &"[redacted]")
                .finish(),
            Self::DisableCws => f.write_str("DisableCws"),
            Self::ForceCi => f.write_str("ForceCi"),
        }
    }
}

/// Wipes a decrypted SAC buffer on every return path.
struct WipeBuffer(Vec<u8>);

impl std::ops::Deref for WipeBuffer {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for WipeBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for WipeBuffer {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl CcState {
    /// `CIPLUS_APP_CC_create` state: paths, slot identity and the URI bitmask.
    pub fn new(
        cert_dir: String,
        cache_dir: String,
        slot_id: u32,
        ci_name: Vec<u8>,
        uri_mask: i32,
    ) -> Self {
        Self {
            elements: Elements::new(),
            dhsk: [0; 256],
            ks_host: [0; 32],
            sek: [0; 16],
            sak: [0; 16],
            akh_index: 0,
            dh_exp: [0; 256],
            cert_ctx: None,
            rsa_device_key: None,
            cert_dir,
            cache_dir,
            slot_id,
            ci_name,
            uri_mask,
            rng: Box::new(OsRng),
        }
    }

    pub fn set_paths(&mut self, cert_dir: String, cache_dir: String) {
        self.cert_dir = cert_dir;
        self.cache_dir = cache_dir;
    }

    pub fn set_uri_mask(&mut self, uri_mask: i32) {
        self.uri_mask = uri_mask;
    }

    pub fn set_ci_name(&mut self, ci_name: Vec<u8>) {
        self.ci_name = ci_name;
    }

    fn authdata_path(&self) -> std::path::PathBuf {
        Path::new(&authdata_filename(
            &self.cache_dir,
            self.slot_id,
            &self.ci_name,
        ))
        .to_path_buf()
    }

    /// One CC APDU, answered with the APDUs and device changes it causes.
    pub fn handle(&mut self, tag: u32, body: &[u8]) -> Vec<CcAction> {
        ms_log::log!(
            "RECV ciplus cc msg CAM{}, tag {:x} len {} dt_id {}",
            self.slot_id,
            tag,
            body.len(),
            body.get(2).map_or(-1, |&b| i32::from(b))
        );
        match tag {
            CIPLUS_TAG_CC_OPEN_REQ => {
                self.data_initialize();
                ms_log::logm!("SEND ------------ CC_OPEN_CNF----------- ");
                vec![CcAction::Apdu {
                    tag: CIPLUS_TAG_CC_OPEN_CNF,
                    body: vec![0x01],
                }]
            }
            CIPLUS_TAG_CC_DATA_REQ => self.data_req(body),
            CIPLUS_TAG_CC_SYNC_REQ => {
                ms_log::logm!("SEND ------------ CC_SYNC_CNF----------- ");
                vec![CcAction::Apdu {
                    tag: CIPLUS_TAG_CC_SYNC_CNF,
                    body: vec![0],
                }]
            }
            CIPLUS_TAG_CC_SAC_DATA_REQ => self.sac_data_req(body),
            CIPLUS_TAG_CC_SAC_SYNC_REQ => self.sac_sync_req(body),
            _ => {
                ms_log::log!(
                    "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!! unknown cc tag {:x} len {}",
                    tag,
                    body.len()
                );
                Vec::new()
            }
        }
    }

    /// `data_initialize`: wipes every credential, then advertises the host.
    fn data_initialize(&mut self) {
        self.reset();
        self.elements.set(30, &[0]);
        let mut versions = [0u8; 32];
        versions[31] = (self.uri_mask & 0xff) as u8;
        ms_log::log!("uri version bitmask set to '{}'", versions[31]);
        self.elements.set(29, &versions);

        self.akh_index = 0;
        let (akh, host_id) = match get_authdata(&self.authdata_path(), 0) {
            Some(record) => {
                self.dhsk = record.dhsk;
                (record.akh, record.host_id)
            }
            None => {
                // DEV-271: C++ passes an uninitialised host_id here.
                self.akh_index = 5;
                ([0u8; 32], [0u8; 8])
            }
        };
        self.elements.set(22, &akh);
        self.elements.set(5, &host_id);
        self.elements.set(49, &[0]);
    }

    /// The `memset(&d->private_data, 0, ...)` at the top of `data_initialize`.
    fn reset(&mut self) {
        self.elements.init();
        wipe(&mut self.dhsk);
        wipe(&mut self.ks_host);
        wipe(&mut self.sek);
        wipe(&mut self.sak);
        wipe(&mut self.dh_exp);
        self.akh_index = 0;
        self.cert_ctx = None;
        self.rsa_device_key = None;
    }

    /// `import_ci_certificates`: imports element `id` from the CAM.
    fn import_ci_certificates(&mut self, id: usize) -> Option<openssl::x509::X509> {
        let data = self.elements.get(id).unwrap_or(&[]);
        let Some(ctx) = self.cert_ctx.as_mut() else {
            ms_log::log!("cannot read/verify DER cert");
            return None;
        };
        let cert = ctx.import_and_check(data);
        if cert.is_none() {
            ms_log::log!("cannot read/verify DER cert");
        }
        cert
    }

    /// `check_ci_certificates`: verifies the CAM chain and stores its id.
    fn check_ci_certificates(&mut self) -> i32 {
        if !self.elements.valid(8) {
            ms_log::log!("CICAM brand cert invalid");
            return -1;
        }
        if !self.elements.valid(16) {
            ms_log::log!("CICAM device cert invalid");
            return -1;
        }
        // The `extract_ci_cert` branch is a static 0 in C++: dead code.
        if self.import_ci_certificates(8).is_none() {
            ms_log::log!("can not import brand cert");
            return -1;
        }
        let Some(device) = self.import_ci_certificates(16) else {
            ms_log::log!("can not import device cert");
            return -1;
        };
        let Some(cicam_id) = hostid_from_cert(&device) else {
            ms_log::log!("can not set cicam_id in elements");
            return -1;
        };
        ms_log::log!("CICAMID: {}", hex_id(&cicam_id));
        if !self.elements.set(6, &cicam_id) {
            ms_log::log!("can not set cicam_id in elements");
            return -1;
        }
        0
    }

    /// `generate_akh`: `sha256(cicam_id || host_id || DHSK)`.
    fn generate_akh(&mut self) {
        let parts: [&[u8]; 3] = [
            self.elements.get(6).unwrap_or(&[]),
            self.elements.get(5).unwrap_or(&[]),
            &self.dhsk,
        ];
        let akh = sha256(&parts);
        self.elements.set(22, &akh);
    }

    /// `check_dh_challenge`: derives DHSK and AKH and caches them.
    fn check_dh_challenge(&mut self) -> i32 {
        ms_log::log!("checking ...");
        if !self.elements.valid(19) {
            ms_log::log!("auth nonce invalid");
            return 0;
        }
        if !self.elements.valid(6) {
            ms_log::log!("cicam id invalid");
            return 0;
        }
        if !self.elements.valid(14) {
            ms_log::log!("dphm invalid");
            return 0;
        }
        if !self.elements.valid(18) {
            ms_log::log!("signature B invalid");
            return 0;
        }
        let dhpm: [u8; 256] = self
            .elements
            .get(14)
            .unwrap_or(&[])
            .try_into()
            .unwrap_or([0; 256]);
        let Some(mut dhsk) = dh_mod_exp(&dhpm, &self.dh_exp, &DH_P, 256) else {
            return 0;
        };
        self.dhsk.copy_from_slice(&dhsk);
        wipe(&mut dhsk);

        self.generate_akh();
        self.akh_index = 5;

        ms_log::log!("writing authdata ...");
        let host_id: [u8; 8] = self
            .elements
            .get(5)
            .and_then(|data| data.try_into().ok())
            .unwrap_or([0; 8]);
        let akh: [u8; 32] = self
            .elements
            .get(22)
            .and_then(|data| data.try_into().ok())
            .unwrap_or([0; 32]);
        let record = Record {
            host_id,
            dhsk: self.dhsk,
            akh,
        };
        write_authdata(&self.authdata_path(), &record);
        1
    }

    /// `generate_key_seed`: `sha256(DHSK[240..], AKH, Ns_host, Ns_module)`.
    fn generate_key_seed(&mut self) {
        let parts: [&[u8]; 4] = [
            &self.dhsk[240..],
            self.elements.get(22).unwrap_or(&[]),
            self.elements.get(20).unwrap_or(&[]),
            self.elements.get(21).unwrap_or(&[]),
        ];
        self.ks_host = sha256(&parts);
    }

    /// `generate_ns_host`: element 20 is a fresh 8-byte host nonce.
    fn generate_ns_host(&mut self) {
        let mut buf = [0u8; 8];
        self.rng.fill(&mut buf);
        self.elements.set(20, &buf);
    }

    /// `generate_SAK_SEK`: the SAC keys from the key seed.
    fn generate_sak_sek(&mut self) {
        let (sak, sek) = generate_sak_sek(&self.ks_host);
        self.sak = sak;
        self.sek = sek;
    }

    /// `restart_dh_challenge`: loads the host chain and sends a new DHPH.
    fn restart_dh_challenge(&mut self) -> i32 {
        ms_log::log!(".... rechecking ...");
        if self.cert_ctx.is_none() {
            self.cert_ctx = Some(CertCtx::new());
        }
        let root_path = format!("{}/root.pem", self.cert_dir);
        let customer_path = format!("{}/customer.pem", self.cert_dir);
        let device_path = format!("{}/device.pem", self.cert_dir);
        let (customer, device) = {
            let ctx = self.cert_ctx.as_mut().expect("created above");
            let _ = ctx.load_and_check(&root_path);
            let customer = ctx.load_and_check(&customer_path).ok().flatten();
            let device = ctx.load_and_check(&device_path).ok().flatten();
            (customer, device)
        };
        let (Some(customer), Some(device)) = (customer, device) else {
            ms_log::log!("can not check loader certificates");
            return -1;
        };

        if let Some(der) = cert_der(&customer)
            && !self.elements.set(7, &der)
        {
            ms_log::log!("can not store cert in elements");
        }
        if let Some(der) = cert_der(&device)
            && !self.elements.set(15, &der)
        {
            ms_log::log!("can not store cert in elements");
        }
        if let Some(host_id) = hostid_from_cert(&device) {
            ms_log::log!("HOSTID: {}", hex_id(&host_id));
            if !self.elements.set(5, &host_id) {
                ms_log::log!("can not set hostid in elements");
            }
        }

        self.rsa_device_key = private_key_from_pem(&device_path);
        if self.rsa_device_key.is_none() {
            ms_log::log!("can not read private key");
            return -1;
        }

        self.elements.invalidate(6);
        self.elements.invalidate(14);
        self.elements.invalidate(18);
        self.elements.invalidate(22);

        let mut exp = [0u8; 256];
        self.rng.fill(&mut exp);
        self.dh_exp = exp;
        wipe(&mut exp);
        let Some(dhph) = dh_mod_exp(&DH_G, &self.dh_exp, &DH_P, 256) else {
            return -1;
        };
        let dhph: [u8; 256] = dhph.try_into().unwrap_or([0; 256]);
        self.elements.set(13, &dhph);

        let Some(nonce) = self
            .elements
            .get(19)
            .and_then(|data| <[u8; 32]>::try_from(data).ok())
        else {
            ms_log::log!("auth nonce invalid");
            return -1;
        };
        if let Some(key) = self.rsa_device_key.as_ref()
            && let Some(signature) = dhph_signature(&nonce, &dhph, key)
        {
            self.elements.set(17, &signature);
        }
        0
    }

    /// `generate_uri_confirm`: element 27 from element 25 and the SAK. A
    /// missing element 25 hashes as empty, like the C++ pointer result.
    fn generate_uri_confirm(&mut self) {
        let uri = self.elements.get(25).unwrap_or(&[]);
        let confirm = uri_confirm(uri, &self.sak);
        self.elements.set(27, &confirm);
    }

    /// `check_new_key`: (parity, key, iv) from the precursor, once.
    fn check_new_key(&mut self) -> Option<(u8, [u8; 16], [u8; 16])> {
        if !self.elements.valid(12) {
            ms_log::log!("key precursor invalid");
            return None;
        }
        if !self.elements.valid(28) {
            ms_log::log!("slot(key register) invalid");
            return None;
        }
        let mut precursor: [u8; 32] = self
            .elements
            .get(12)
            .and_then(|data| data.try_into().ok())
            .unwrap_or([0; 32]);
        let slot = self.elements.get(28).and_then(|data| data.first()).copied();
        let (key, iv) = key_from_precursor(&precursor);
        wipe(&mut precursor);
        self.elements.invalidate(12);
        self.elements.invalidate(28);

        // DEV-272: C++ indexes d->key[slot] without a bound check.
        let Some(slot) = slot.filter(|&slot| slot <= 1) else {
            ms_log::log!(
                "invalid key register (0-even, 1-odd) from CI+ CAM {}",
                self.slot_id
            );
            return None;
        };
        ms_log::logm!(
            "=== descrambler_set_key === adapter = CA{} key register (0-even, 1-odd) = {}",
            self.slot_id,
            slot
        );
        // DEV-274: C++ prints the key and the IV; only the id and parity stay.
        ms_log::log!("received from CI+ CAM {}: parity {}", self.slot_id, slot);
        Some((slot, key, iv))
    }

    /// `data_get_handle_new`: the per-element triggers.
    fn data_get_handle_new(&mut self, id: usize, actions: &mut Vec<CcAction>) {
        match id {
            8 => {
                if self.check_ci_certificates() == 0 {
                    self.check_dh_challenge();
                }
            }
            19 => {
                if self.restart_dh_challenge() != 0 {
                    actions.push(CcAction::ForceCi);
                }
            }
            21 => {
                self.generate_ns_host();
                self.generate_key_seed();
                self.generate_sak_sek();
            }
            28 => {
                if let Some((parity, key, iv)) = self.check_new_key() {
                    actions.push(CcAction::NewKey { parity, key, iv });
                }
            }
            25 => self.generate_uri_confirm(),
            6 | 12 | 14 | 16 | 18 | 26 => {
                ms_log::logm!("not need to be handled id {}", id);
            }
            _ => ms_log::log!("unhandled id {}", id),
        }
    }

    /// `data_req_handle_new`: a cached AKH is consumed once per request.
    fn data_req_handle_new(&mut self, id: usize) {
        if id != 22 || self.akh_index == 5 {
            return;
        }
        let index = self.akh_index as usize;
        self.akh_index += 1;
        let (akh, host_id) = match get_authdata(&self.authdata_path(), index) {
            Some(record) => {
                self.dhsk = record.dhsk;
                (record.akh, record.host_id)
            }
            None => {
                self.akh_index = 5;
                ([0u8; 32], [0u8; 8])
            }
        };
        self.elements.set(22, &akh);
        self.elements.set(5, &host_id);
    }

    /// `data_get_loop`: sets the sent elements; `None` when the body is short.
    fn data_get_loop(
        &mut self,
        data: &[u8],
        items: usize,
        actions: &mut Vec<CcAction>,
    ) -> Option<usize> {
        let mut pos = 0;
        for _ in 0..items {
            let header = data.get(pos..pos + 3)?;
            let id = usize::from(header[0]);
            let len = usize::from(u16::from_be_bytes([header[1], header[2]]));
            pos += 3;
            let item = data.get(pos..pos + len)?;
            ms_log::logm!("set element(dt_id) {} dt_len = {}", id, len);
            self.elements.set(id, item);
            self.data_get_handle_new(id, actions);
            pos += len;
        }
        Some(pos)
    }

    /// `data_req_loop`: serialises every requested element; `None` when the
    /// request is empty or one element is missing.
    fn data_req_loop(&mut self, data: &[u8], items: usize) -> Option<Vec<u8>> {
        // C++ treats an empty request loop as "cannot req data": no answer.
        if items == 0 || items > data.len() {
            return None;
        }
        let mut out = Vec::new();
        for &id in &data[..items] {
            let id = usize::from(id);
            ms_log::logm!("req element {}", id);
            self.data_req_handle_new(id);
            match self.elements.get(id) {
                Some(bytes) if !bytes.is_empty() => {}
                _ => {
                    ms_log::log!("cannot get element {}", id);
                    return None;
                }
            }
            if !self.elements.get_req(id, &mut out) {
                return None;
            }
        }
        Some(out)
    }

    /// `ci_ccmgr_cc_data_req`.
    fn data_req(&mut self, body: &[u8]) -> Vec<CcAction> {
        let mut actions = Vec::new();
        let Some((&id_bitmask, rest)) = body.split_first() else {
            return actions;
        };
        let Some((&send_count, rest)) = rest.split_first() else {
            return actions;
        };
        let Some(consumed) = self.data_get_loop(rest, usize::from(send_count), &mut actions) else {
            return actions;
        };
        let mut rp = consumed;
        let Some(&req_count) = rest.get(rp) else {
            return actions;
        };
        rp += 1;
        let Some(items) = self.data_req_loop(rest.get(rp..).unwrap_or(&[]), usize::from(req_count))
        else {
            ms_log::log!("cannot req data");
            return actions;
        };

        let mut dest = Vec::with_capacity(2 + items.len());
        dest.push(id_bitmask);
        dest.push(req_count);
        dest.extend_from_slice(&items);
        ms_log::logm!("SEND ------------ CC_DATA_CNF----------- ");
        actions.push(CcAction::Apdu {
            tag: CIPLUS_TAG_CC_DATA_CNF,
            body: dest,
        });
        actions
    }

    /// `ci_ccmgr_cc_sac_send`: pads, seals and writes one SAC APDU.
    fn sac_send(&self, tag: u32, mut data: Vec<u8>) -> Option<CcAction> {
        if data.len() < 8 {
            return None;
        }
        add_padding(&mut data, 8, 16);
        let len = (data.len() - 8) as u16;
        data[6..8].copy_from_slice(&len.to_be_bytes());

        let mac = sac_gen_auth(&self.sak, &data);
        data.extend_from_slice(&mac);
        // The header stays clear; cipher and MAC cover everything after it.
        sac_crypt(&self.sek, &mut data[8..], true);
        Some(CcAction::Apdu { tag, body: data })
    }

    /// `ci_ccmgr_cc_sac_data_req`.
    fn sac_data_req(&mut self, body: &[u8]) -> Vec<CcAction> {
        let mut actions = Vec::new();
        // DEV-273: C++ only checks len < 10 and then feeds CBC partial blocks.
        if body.len() < 10 || !(body.len() - 8).is_multiple_of(16) {
            return actions;
        }
        let mut data = WipeBuffer(body.to_vec());
        sac_crypt(&self.sek, &mut data[8..], false);
        if !sac_check_auth(&self.sak, &data) {
            ms_log::log!("check_auth of message failed");
            return actions;
        }

        ms_log::logm!(
            "serial sac data req: {}",
            u32::from_be_bytes([data[0], data[1], data[2], data[3]])
        );
        actions.push(CcAction::DisableCws);

        let serial = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let id_bitmask = data[8];
        let send_count = usize::from(data[9]);
        let Some(consumed) = self.data_get_loop(&data[10..], send_count, &mut actions) else {
            return actions;
        };
        let mut rp = 10 + consumed;
        if data.len() < rp + 1 {
            return actions;
        }
        let req_count = data[rp];
        rp += 1;
        let Some(items) = self.data_req_loop(&data[rp..], usize::from(req_count)) else {
            ms_log::log!("cannot req data");
            return actions;
        };

        let mut dest = Vec::with_capacity(10 + items.len());
        dest.extend_from_slice(&serial.to_be_bytes());
        dest.extend_from_slice(&0x0100_0000u32.to_be_bytes());
        dest.push(id_bitmask);
        dest.push(req_count);
        dest.extend_from_slice(&items);
        ms_log::logm!("SEND ------------ CC_SAC_DATA_CNF----------- ");
        if let Some(action) = self.sac_send(CIPLUS_TAG_CC_SAC_DATA_CNF, dest) {
            actions.push(action);
        }
        actions
    }

    /// `ci_ccmgr_cc_sac_sync_req`.
    fn sac_sync_req(&mut self, body: &[u8]) -> Vec<CcAction> {
        let mut actions = Vec::new();
        // DEV-273: C++ reads the serial without any length check.
        if body.len() < 8 {
            return actions;
        }
        let serial = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
        let mut dest = Vec::with_capacity(9);
        dest.extend_from_slice(&serial.to_be_bytes());
        dest.extend_from_slice(&0x0100_0000u32.to_be_bytes());
        dest.push(0);
        ms_log::log!("SEND ------------ CC_SAC_SYNC_CNF----------- ");
        if let Some(action) = self.sac_send(CIPLUS_TAG_CC_SAC_SYNC_CNF, dest) {
            actions.push(action);
        }
        actions
    }
}

impl fmt::Debug for CcState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CcState")
            .field("akh_index", &self.akh_index)
            .field("elements", &self.elements)
            .finish()
    }
}

impl Drop for CcState {
    fn drop(&mut self) {
        wipe(&mut self.dhsk);
        wipe(&mut self.ks_host);
        wipe(&mut self.sek);
        wipe(&mut self.sak);
        wipe(&mut self.dh_exp);
    }
}

fn hex_id(id: &[u8; 8]) -> String {
    id.iter().map(|byte| format!("{byte:02X}")).collect()
}
