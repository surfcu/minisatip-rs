//! CI+ certificate store and X.509 checks (`certificate_*`, ca.cpp:495-640).

use std::io;

use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::stack::Stack;
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::verify::X509VerifyFlags;
use openssl::x509::{X509, X509StoreContext};

use super::crypto::str2bin;
use super::ffi;

/// `struct cert_ctx`: the trusted certificates. C++ keeps an `X509_STORE`;
/// the `openssl` crate cannot add to a built store, so one is built per check.
#[derive(Default)]
pub struct CertCtx {
    trusted: Vec<X509>,
    /// Equivalent to C++'s `ctx->store != NULL`.
    root_loaded: bool,
    /// Verify error left by the last `check`, for the tests.
    #[cfg(test)]
    last_error: std::cell::Cell<i32>,
}

fn invalid(error: openssl::error::ErrorStack) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

impl CertCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// `X509_STORE_add_cert`: keeps one copy of each certificate chain.
    fn trust(&mut self, cert: &X509) {
        let Ok(der) = cert.to_der() else {
            return;
        };
        if self
            .trusted
            .iter()
            .any(|known| known.to_der().is_ok_and(|known| known == der))
        {
            return;
        }
        self.trusted.push(cert.clone());
    }

    /// `certificate_validate`: builds the store, installs C++'s verify callback
    /// plus `X509_V_FLAG_IGNORE_CRITICAL`, then verifies.
    fn check(&self, cert: &X509) -> bool {
        let Ok(mut builder) = X509StoreBuilder::new() else {
            return false;
        };
        if builder.set_flags(X509VerifyFlags::IGNORE_CRITICAL).is_err() {
            return false;
        }
        for trusted in &self.trusted {
            if builder.add_cert(trusted.clone()).is_err() {
                return false;
            }
        }
        let store = builder.build();
        let Ok(chain) = Stack::new() else {
            return false;
        };
        let Ok(mut context) = X509StoreContext::new() else {
            return false;
        };
        #[cfg(test)]
        ffi::reset_callback_error();
        let result = context.init(&store, cert, &chain, |ctx| {
            ffi::install(ctx);
            match ctx.verify_cert() {
                Ok(true) => Ok(true),
                Ok(false) => {
                    ms_log::log!("{}", ctx.error().error_string());
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        });
        // OpenSSL normalizes a callback-rejected OK to UNSPECIFIED after the
        // call, so record the error the callback itself saw.
        #[cfg(test)]
        self.last_error.set(ffi::last_callback_error());
        result.unwrap_or(false)
    }

    /// `certificate_load_and_check`: the first call trusts the root file and
    /// returns `None`; later calls verify and trust it (a bad root is `Err`).
    pub fn load_and_check(&mut self, path: &str) -> io::Result<Option<X509>> {
        let data = std::fs::read(path)?;

        if !self.root_loaded {
            let certs = X509::stack_from_pem(&data).map_err(invalid)?;
            if certs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "no certificate in root file",
                ));
            }
            for cert in &certs {
                self.trust(cert);
            }
            self.root_loaded = true;
            return Ok(None);
        }

        let cert = X509::from_pem(&data).map_err(invalid)?;
        if !self.check(&cert) {
            ms_log::log!("can not vaildate certificate");
            return Ok(None);
        }
        let certs = X509::stack_from_pem(&data).map_err(invalid)?;
        for cert in &certs {
            self.trust(cert);
        }
        Ok(Some(cert))
    }

    /// `certificate_import_and_check`: DER certificate, verify then trust.
    pub fn import_and_check(&mut self, der: &[u8]) -> Option<X509> {
        let cert = match X509::from_der(der) {
            Ok(cert) => cert,
            Err(_) => {
                ms_log::log!("can not read certificate");
                return None;
            }
        };
        if !self.check(&cert) {
            ms_log::log!("can not vaildate certificate");
            return None;
        }
        self.trust(&cert);
        Some(cert)
    }
}

/// `element_set_hostid_from_certificate`: the 16-character subject CN,
/// decoded as hex. C++ truncates the CN to 19 characters first.
pub fn hostid_from_cert(cert: &X509) -> Option<[u8; 8]> {
    let entry = cert.subject_name().entries_by_nid(Nid::COMMONNAME).next()?;
    let data = entry.data().as_slice();
    let mut hostid = [0u8; 20];
    let len = data.len().min(hostid.len() - 1);
    hostid[..len].copy_from_slice(&data[..len]);
    let end = hostid
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(hostid.len());
    if end != 16 {
        ms_log::log!("malformed hostid");
        return None;
    }
    str2bin(&hostid[..16]).try_into().ok()
}

/// `i2d_X509`: DER encoding of a certificate.
pub fn cert_der(cert: &X509) -> Option<Vec<u8>> {
    cert.to_der().ok()
}

/// `rsa_privatekey_open`: reads the private key from a PEM file.
pub fn private_key_from_pem(path: &str) -> Option<PKey<Private>> {
    let data = std::fs::read(path).ok()?;
    PKey::private_key_from_pem(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::en50221::ciplus::test_pki::*;
    use openssl::asn1::{Asn1Object, Asn1OctetString, Asn1Time};
    use openssl::x509::X509Extension;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn load_root_then_host_chain() {
        let pki = pki();
        let dir = temp_dir("host");
        let root = dir.join("root.pem");
        let brand = dir.join("brand.pem");
        let device = dir.join("device.pem");
        let device_key = pki.host_device_key.private_key_to_pem_pkcs8().unwrap();
        write_pem(&root, &[&cert_pem(&pki.root)]);
        write_pem(&brand, &[&cert_pem(&pki.host_brand)]);
        // Like a real device.pem: the certificate, then its private key.
        write_pem(&device, &[&cert_pem(&pki.host_device), &device_key]);

        let mut ctx = CertCtx::new();
        assert!(ctx.load_and_check(path_arg(&root)).unwrap().is_none());
        assert!(ctx.load_and_check(path_arg(&brand)).unwrap().is_some());
        let device = ctx.load_and_check(path_arg(&device)).unwrap().unwrap();
        assert_eq!(
            hostid_from_cert(&device),
            Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn import_cam_chain() {
        let pki = pki();
        let dir = temp_dir("cam");
        let root = dir.join("root.pem");
        write_pem(&root, &[&cert_pem(&pki.root)]);

        let mut ctx = CertCtx::new();
        ctx.load_and_check(path_arg(&root)).unwrap();
        let brand = ctx
            .import_and_check(&pki.cam_brand.to_der().unwrap())
            .expect("CAM brand cert");
        assert_eq!(hostid_from_cert(&brand), None, "brand CN is not a host id");
        assert!(
            ctx.import_and_check(&pki.cam_device.to_der().unwrap())
                .is_some()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn device_from_another_root_is_rejected() {
        let pki = pki();
        let dir = temp_dir("other");
        let root = dir.join("root.pem");
        write_pem(&root, &[&cert_pem(&pki.root)]);

        let mut ctx = CertCtx::new();
        ctx.load_and_check(path_arg(&root)).unwrap();
        assert!(
            ctx.import_and_check(&pki.other_device.to_der().unwrap())
                .is_none()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn valid_dated_root_is_rejected() {
        let pki = pki();
        let dir = temp_dir("valid-root");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let not_before = Asn1Time::from_unix(now - 86_400).unwrap();
        let not_after = Asn1Time::from_unix(now + 10 * 365 * 86_400).unwrap();
        let (expired_before, expired_after) = expired();
        let root = build_cert(
            "Valid Root",
            &pki.root_key,
            "Valid Root",
            &pki.root_key,
            None,
            true,
            90,
            &not_before,
            &not_after,
        );
        let leaf = build_cert(
            "0123456789ABCDEF",
            &pki.host_device_key,
            "Valid Root",
            &pki.root_key,
            Some(&root),
            false,
            91,
            &expired_before,
            &expired_after,
        );
        let root_path = dir.join("root.pem");
        let leaf_path = dir.join("leaf.pem");
        write_pem(&root_path, &[&cert_pem(&root)]);
        write_pem(&leaf_path, &[&cert_pem(&leaf)]);

        let mut ctx = CertCtx::new();
        assert!(ctx.load_and_check(path_arg(&root_path)).unwrap().is_none());
        assert!(
            ctx.load_and_check(path_arg(&leaf_path)).unwrap().is_none(),
            "C++ verify_cb rejects certificates that are not expired"
        );
        assert_eq!(
            ctx.last_error.get(),
            openssl_sys::X509_V_OK,
            "the callback rejected a valid-dated certificate without a verify error"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_critical_extension_is_ignored() {
        let pki = pki();
        let dir = temp_dir("critical-ext");
        let (not_before, not_after) = expired();
        let oid = Asn1Object::from_str("1.3.6.1.4.1.99999.1").unwrap();
        let value = Asn1OctetString::new_from_bytes(b"private").unwrap();
        let extra = X509Extension::new_from_der(&oid, true, &value).unwrap();

        let brand_key = key();
        let brand = build_cert(
            "Critical Extension Brand",
            &brand_key,
            "Test Root",
            &pki.root_key,
            Some(&pki.root),
            true,
            92,
            &not_before,
            &not_after,
        );
        let device = build_cert_with(
            "0123456789ABCDEF",
            &pki.host_device_key,
            "Critical Extension Brand",
            &brand_key,
            Some(&brand),
            false,
            true,
            Some(extra),
            93,
            &not_before,
            &not_after,
        );

        let root_path = dir.join("root.pem");
        write_pem(&root_path, &[&cert_pem(&pki.root)]);
        let mut ctx = CertCtx::new();
        ctx.load_and_check(path_arg(&root_path)).unwrap();
        assert!(ctx.import_and_check(&brand.to_der().unwrap()).is_some());
        assert!(
            ctx.import_and_check(&device.to_der().unwrap()).is_some(),
            "IGNORE_CRITICAL must skip the unknown critical extension"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn noncritical_ca_constraints_are_accepted() {
        let pki = pki();
        let dir = temp_dir("noncritical-ca");
        let (not_before, not_after) = expired();

        let brand_key = key();
        let brand = build_cert_with(
            "Noncritical Brand",
            &brand_key,
            "Test Root",
            &pki.root_key,
            Some(&pki.root),
            true,
            false,
            None,
            94,
            &not_before,
            &not_after,
        );
        let device = build_cert(
            "0123456789ABCDEF",
            &pki.host_device_key,
            "Noncritical Brand",
            &brand_key,
            Some(&brand),
            false,
            95,
            &not_before,
            &not_after,
        );

        let root_path = dir.join("root.pem");
        write_pem(&root_path, &[&cert_pem(&pki.root)]);
        let mut ctx = CertCtx::new();
        ctx.load_and_check(path_arg(&root_path)).unwrap();
        assert!(
            ctx.import_and_check(&brand.to_der().unwrap()).is_some(),
            "a non-critical BasicConstraints CA is valid without X509_STRICT"
        );
        assert!(ctx.import_and_check(&device.to_der().unwrap()).is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn malformed_hostid_is_rejected() {
        let pki = pki();
        let (not_before, not_after) = expired();
        let cert = build_cert(
            "0123456789ABCDE",
            &pki.host_device_key,
            "Test Root",
            &pki.root_key,
            None,
            false,
            92,
            &not_before,
            &not_after,
        );
        assert_eq!(hostid_from_cert(&cert), None);
    }

    #[test]
    fn private_key_from_device_pem() {
        let pki = pki();
        let dir = temp_dir("key");
        let device = dir.join("device.pem");
        let key_pem = pki.host_device_key.private_key_to_pem_pkcs8().unwrap();
        write_pem(&device, &[&cert_pem(&pki.host_device), &key_pem]);

        let key = private_key_from_pem(path_arg(&device)).expect("device key");
        let cert_key = pki.host_device.public_key().unwrap();
        assert_eq!(
            key.public_key_to_der().unwrap(),
            cert_key.public_key_to_der().unwrap()
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
