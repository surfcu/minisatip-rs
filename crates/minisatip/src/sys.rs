//! Signal handling. The only unsafe code in the binary lives here.

#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    // Async-signal-safe: only an atomic store.
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Installs handlers for SIGINT and SIGTERM.
pub fn install_shutdown_handlers() {
    // SAFETY: the handler only touches an atomic and `signal` accepts any
    // function with the C signal signature.
    unsafe {
        let handler = on_signal as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// Whether SIGINT or SIGTERM has been received.
pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}
