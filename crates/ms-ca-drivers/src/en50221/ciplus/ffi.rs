//! Raw OpenSSL store-context calls that the `openssl` crate does not expose.
#![allow(unsafe_code)]

use openssl::x509::X509StoreContextRef;
use openssl_sys::X509_STORE_CTX;
use std::os::raw::c_int;

type VerifyCallback = unsafe extern "C" fn(c_int, *mut X509_STORE_CTX) -> c_int;

unsafe extern "C" {
    fn X509_STORE_CTX_set_verify_cb(ctx: *mut X509_STORE_CTX, cb: Option<VerifyCallback>);
}

#[cfg(test)]
thread_local! {
    static LAST_CALLBACK_ERROR: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// The error the verify callback saw last, for the tests.
#[cfg(test)]
pub(super) fn last_callback_error() -> i32 {
    LAST_CALLBACK_ERROR.with(std::cell::Cell::get)
}

/// Clears the recorded callback error before a check.
#[cfg(test)]
pub(super) fn reset_callback_error() {
    LAST_CALLBACK_ERROR.with(|error| error.set(-1));
}

/// C++ `verify_cb`: ignores `ok` and accepts only expired/not-yet-valid
/// certificates, which is how the real CI+ chains verify.
unsafe extern "C" fn verify_cb(_ok: c_int, ctx: *mut X509_STORE_CTX) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    // SAFETY: OpenSSL passes a live store context that is non-null here.
    let error = unsafe { openssl_sys::X509_STORE_CTX_get_error(ctx) };
    #[cfg(test)]
    LAST_CALLBACK_ERROR.with(|last| last.set(error));
    c_int::from(
        error == openssl_sys::X509_V_ERR_CERT_NOT_YET_VALID
            || error == openssl_sys::X509_V_ERR_CERT_HAS_EXPIRED,
    )
}

/// Installs `verify_cb` on a store context.
pub(super) fn install(ctx: &mut X509StoreContextRef) {
    // SAFETY: a `ForeignTypeRef` wrapper is the C struct itself (this is the
    // cast `ForeignTypeRef::as_ptr` makes) and lives for this call.
    let ptr = ctx as *mut X509StoreContextRef as *mut X509_STORE_CTX;
    // SAFETY: ptr refers to the caller's live context for the duration.
    unsafe {
        X509_STORE_CTX_set_verify_cb(ptr, Some(verify_cb));
    }
}

/// Overwrites `buf` with zeros in a way the compiler must not elide.
pub(super) fn wipe(buf: &mut [u8]) {
    for byte in buf {
        // SAFETY: `byte` is a valid, aligned pointer to one writable `u8`.
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}
