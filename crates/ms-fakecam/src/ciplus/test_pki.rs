//! Throwaway expired certificate chain for the host-side CI+ tests.
//!
//! Writes the host credentials into `root.pem`, `customer.pem` and `device.pem`
//! and returns the matching CAM configuration, so both sides verify the same
//! root and device chain.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::rsa::Rsa;
use openssl::x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, KeyUsage, SubjectKeyIdentifier,
};
use openssl::x509::{X509, X509Builder, X509Name, X509NameBuilder};

use super::CiPlusCam;

/// Real root certificate shape: not yet valid and expired.
const NOT_BEFORE: i64 = 1_227_657_600; // 2008-11-26
const NOT_AFTER: i64 = 946_598_400; // 1999-12-31

/// Host certificate directory, authdata cache directory and matching CAM
/// credentials.
pub struct TestPki {
    /// Directory holding `root.pem`, `customer.pem` and `device.pem`.
    pub cert_dir: PathBuf,
    /// Empty directory for the CAM's authdata cache files.
    pub cache_dir: PathBuf,
    /// The CAM side of the same chain.
    pub cam: CiPlusCam,
}

impl TestPki {
    /// Removes both temporary directories.
    pub fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.cert_dir);
        let _ = std::fs::remove_dir_all(&self.cache_dir);
    }
}

fn key() -> PKey<Private> {
    PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap()
}

fn name(cn: &str) -> X509Name {
    let mut builder = X509NameBuilder::new().unwrap();
    builder.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
    builder.build()
}

#[allow(clippy::too_many_arguments)]
fn build_cert(
    subject_cn: &str,
    subject_key: &PKey<Private>,
    issuer_cn: &str,
    issuer_key: &PKey<Private>,
    issuer_cert: Option<&X509>,
    is_ca: bool,
    serial: u32,
    not_before: &Asn1Time,
    not_after: &Asn1Time,
) -> X509 {
    let mut builder = X509Builder::new().unwrap();
    builder.set_version(2).unwrap();
    let serial = Asn1Integer::from_bn(&BigNum::from_u32(serial).unwrap()).unwrap();
    builder.set_serial_number(&serial).unwrap();
    builder.set_subject_name(&name(subject_cn)).unwrap();
    builder.set_issuer_name(&name(issuer_cn)).unwrap();
    builder.set_not_before(not_before).unwrap();
    builder.set_not_after(not_after).unwrap();
    builder.set_pubkey(subject_key).unwrap();
    {
        let context = builder.x509v3_context(issuer_cert.map(|c| c.as_ref()), None);
        let ski = SubjectKeyIdentifier::new().build(&context).unwrap();
        builder.append_extension(ski).unwrap();
    }
    if issuer_cert.is_some() {
        let context = builder.x509v3_context(issuer_cert.map(|c| c.as_ref()), None);
        let aki = AuthorityKeyIdentifier::new()
            .keyid(true)
            .build(&context)
            .unwrap();
        builder.append_extension(aki).unwrap();
    }
    if is_ca {
        let mut constraints = BasicConstraints::new();
        constraints.ca().critical();
        builder
            .append_extension(constraints.build().unwrap())
            .unwrap();
        builder
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .key_cert_sign()
                    .crl_sign()
                    .build()
                    .unwrap(),
            )
            .unwrap();
    }
    builder.sign(issuer_key, MessageDigest::sha256()).unwrap();
    builder.build()
}

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ms-fakecam-pki-{}-{tag}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_pem(path: &Path, parts: &[&[u8]]) {
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
    }
    std::fs::write(path, data).unwrap();
}

fn cert_pem(cert: &X509) -> Vec<u8> {
    cert.to_pem().unwrap()
}

/// Builds the expired test chain and writes the host files into a fresh
/// directory named after `tag`.
pub fn pki(tag: &str) -> TestPki {
    pki_with_device(tag, true)
}

/// Like [`pki`], but without `device.pem` when `with_device` is false, for the
/// plain-CI fallback (no host key).
pub fn pki_with_device(tag: &str, with_device: bool) -> TestPki {
    let not_before = Asn1Time::from_unix(NOT_BEFORE).unwrap();
    let not_after = Asn1Time::from_unix(NOT_AFTER).unwrap();
    let root_key = key();
    let root = build_cert(
        "Test Root",
        &root_key,
        "Test Root",
        &root_key,
        None,
        true,
        1,
        &not_before,
        &not_after,
    );
    let host_brand_key = key();
    let host_brand = build_cert(
        "Host Brand",
        &host_brand_key,
        "Test Root",
        &root_key,
        Some(&root),
        true,
        2,
        &not_before,
        &not_after,
    );
    let host_device_key = key();
    let host_device = build_cert(
        "0123456789ABCDEF",
        &host_device_key,
        "Host Brand",
        &host_brand_key,
        Some(&host_brand),
        false,
        3,
        &not_before,
        &not_after,
    );
    let cam_brand_key = key();
    let cam_brand = build_cert(
        "CAM Brand",
        &cam_brand_key,
        "Test Root",
        &root_key,
        Some(&root),
        true,
        4,
        &not_before,
        &not_after,
    );
    let cam_device_key = key();
    let cam_device = build_cert(
        "FEDCBA9876543210",
        &cam_device_key,
        "CAM Brand",
        &cam_brand_key,
        Some(&cam_brand),
        false,
        5,
        &not_before,
        &not_after,
    );

    let cert_dir = temp_dir(tag);
    write_pem(&cert_dir.join("root.pem"), &[&cert_pem(&root)]);
    write_pem(&cert_dir.join("customer.pem"), &[&cert_pem(&host_brand)]);
    if with_device {
        write_pem(
            &cert_dir.join("device.pem"),
            &[
                &cert_pem(&host_device),
                &host_device_key.private_key_to_pem_pkcs8().unwrap(),
            ],
        );
    }
    TestPki {
        cert_dir,
        cache_dir: temp_dir(&format!("{tag}-cache")),
        cam: CiPlusCam {
            root_pem: cert_pem(&root),
            brand_cert_der: cam_brand.to_der().unwrap(),
            device_cert_der: cam_device.to_der().unwrap(),
            device_key_pem: cam_device_key.private_key_to_pem_pkcs8().unwrap(),
            ..CiPlusCam::default()
        },
    }
}
