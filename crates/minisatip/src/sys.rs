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

/// Current Unix time in seconds, as returned by `time(NULL)`.
pub fn now_seconds() -> i64 {
    // SAFETY: time(2) accepts a null pointer and returns the current time.
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

/// Current local time formatted like `asctime` with a trailing space
/// (minisatip.cpp:1155-1167).
pub fn asctime_now() -> String {
    // SAFETY: localtime_r and asctime_r write into stack storage owned here,
    // and the C string is copied before the buffer is dropped.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        let mut buffer = [0 as libc::c_char; 32];
        if libc::asctime_r(&tm, buffer.as_mut_ptr()).is_null() {
            return String::new();
        }
        let text = std::ffi::CStr::from_ptr(buffer.as_ptr()).to_string_lossy();
        format!("{} ", text.trim_end_matches('\n'))
    }
}

/// Numeric user id (`getuid`).
pub fn run_user() -> i32 {
    // SAFETY: getuid(2) has no preconditions.
    unsafe { libc::getuid() as i32 }
}

/// Formats `time_running` like minisatip.cpp:1549-1552.
pub fn elapsed_string(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    let secs = seconds % 60;
    let prefix = if days > 0 {
        format!("{days}d ")
    } else {
        String::new()
    };
    format!("{prefix}{hours:02}:{minutes:02}:{secs:02}")
}

#[cfg(test)]
mod tests {
    use super::elapsed_string;

    #[test]
    fn elapsed_matches_cpp_format() {
        // minisatip.cpp:1549-1552: "%.0d%s%02d:%02d:%02d".
        assert_eq!(elapsed_string(0), "00:00:00");
        assert_eq!(elapsed_string(61), "00:01:01");
        assert_eq!(elapsed_string(3 * 86_400 + 3_661), "3d 01:01:01");
    }
}
