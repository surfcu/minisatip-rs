//! Expired certificate chains shared by the CI+ tests (`certs.rs`, `cc.rs`).

use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::rsa::Rsa;
use openssl::x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, KeyUsage, SubjectKeyIdentifier,
};
use openssl::x509::{X509, X509Builder, X509Extension, X509Name, X509NameBuilder};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Real root certificate shape: notYetValid and expired.
pub(crate) const NOT_BEFORE: i64 = 1_227_657_600; // 2008-11-26
pub(crate) const NOT_AFTER: i64 = 946_598_400; // 1999-12-31

pub(crate) struct Pki {
    pub root: X509,
    pub root_key: PKey<Private>,
    pub host_brand: X509,
    pub host_device: X509,
    pub host_device_key: PKey<Private>,
    pub cam_brand: X509,
    pub cam_device: X509,
    pub other_device: X509,
}

pub(crate) fn key() -> PKey<Private> {
    PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap()
}

pub(crate) fn expired() -> (Asn1Time, Asn1Time) {
    (
        Asn1Time::from_unix(NOT_BEFORE).unwrap(),
        Asn1Time::from_unix(NOT_AFTER).unwrap(),
    )
}

fn name(cn: &str) -> X509Name {
    let mut builder = X509NameBuilder::new().unwrap();
    builder.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
    builder.build()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_cert(
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
    build_cert_with(
        subject_cn,
        subject_key,
        issuer_cn,
        issuer_key,
        issuer_cert,
        is_ca,
        true,
        None,
        serial,
        not_before,
        not_after,
    )
}

/// `build_cert` with a choice of critical BasicConstraints and an extra
/// extension, for the IGNORE_CRITICAL and non-strict tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_cert_with(
    subject_cn: &str,
    subject_key: &PKey<Private>,
    issuer_cn: &str,
    issuer_key: &PKey<Private>,
    issuer_cert: Option<&X509>,
    is_ca: bool,
    ca_critical: bool,
    extra: Option<X509Extension>,
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
        constraints.ca();
        if ca_critical {
            constraints.critical();
        }
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
    if let Some(extra) = extra {
        builder.append_extension(extra).unwrap();
    }
    builder.sign(issuer_key, MessageDigest::sha256()).unwrap();
    builder.build()
}

pub(crate) fn pki() -> &'static Pki {
    static PKI: OnceLock<Pki> = OnceLock::new();
    PKI.get_or_init(|| {
        let (not_before, not_after) = expired();
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
        let other_root_key = key();
        let other_root = build_cert(
            "Other Root",
            &other_root_key,
            "Other Root",
            &other_root_key,
            None,
            true,
            6,
            &not_before,
            &not_after,
        );
        let other_device_key = key();
        let other_device = build_cert(
            "0011223344556677",
            &other_device_key,
            "Other Root",
            &other_root_key,
            Some(&other_root),
            false,
            7,
            &not_before,
            &not_after,
        );
        Pki {
            root,
            root_key,
            host_brand,
            host_device,
            host_device_key,
            cam_brand,
            cam_device,
            other_device,
        }
    })
}

pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ms-ciplus-{}-{tag}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(crate) fn write_pem(path: &Path, parts: &[&[u8]]) {
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(part);
    }
    std::fs::write(path, data).unwrap();
}

pub(crate) fn cert_pem(cert: &X509) -> Vec<u8> {
    cert.to_pem().unwrap()
}

pub(crate) fn path_arg(path: &Path) -> &str {
    path.to_str().unwrap()
}
