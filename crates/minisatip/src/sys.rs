//! Signal handling. The only unsafe code in the binary lives here.

#![allow(unsafe_code)]

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};

use ms_log::{Backend, Timestamp};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Mirrors libc's process-wide syslog connection state, so `become_daemon`
/// can close it before its descriptor sweep and the next line reopens it
/// with the C++ options (`minisatip.cpp:1919-1923`).
static SYSLOG_OPEN: AtomicBool = AtomicBool::new(false);

/// C++ `app_name` (`minisatip.cpp:78`).
const APP_NAME: &str = "minisatip";

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
        // utils.cpp:224
        if libc::signal(libc::SIGINT, handler) == libc::SIG_ERR {
            ms_log::log!("Could not set signal SIGINT");
        }
        // utils.cpp:228
        if libc::signal(libc::SIGTERM, handler) == libc::SIG_ERR {
            ms_log::log!("Could not set signal SIGTERM");
        }
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

/// Path of the pid file, `PID_NAME = "/var/run/%s.pid"` with the C++
/// `app_name` (minisatip.cpp:76,1889-1895).
pub fn pid_file_path() -> String {
    "/var/run/minisatip.pid".to_owned()
}

/// Writes the current pid, ignoring failures like C++ (`fopen` checked only
/// for NULL).
pub fn write_pid_file() {
    let _ = std::fs::write(pid_file_path(), format!("{}", std::process::id()));
}

/// Removes the pid file (`unlink(pid_file)`, minisatip.cpp:2021).
pub fn remove_pid_file() {
    let _ = std::fs::remove_file(pid_file_path());
}

/// `becomeDaemon` (utils.cpp:276-350): kill a previous instance recorded in
/// the pid file, double fork, start a new session, close every descriptor and
/// reopen stdin on /dev/null with stdout/stderr on `log_file`.
///
/// Returns the reason on failure; C++ ignores the return value.
pub fn become_daemon(log_file: &str) -> Result<(), String> {
    kill_previous_instance();

    // utils.cpp:300
    ms_log::log!(
        "running {} in background and logging to {}",
        APP_NAME,
        log_file
    );

    // SAFETY: fork(2) has no preconditions; the parent leaves immediately.
    match unsafe { libc::fork() } {
        -1 => return Err("fork failed".to_owned()),
        0 => {}
        _ => {
            // SAFETY: _exit is async-signal-safe and skips destructors, as in C++.
            unsafe { libc::_exit(0) }
        }
    }
    // SAFETY: setsid(2) fails only when the caller is a process group leader.
    if unsafe { libc::setsid() } == -1 {
        return Err("setsid failed".to_owned());
    }
    // SAFETY: the second fork detaches from the session leader.
    match unsafe { libc::fork() } {
        -1 => return Err("fork failed".to_owned()),
        0 => {}
        _ => {
            // SAFETY: see above.
            unsafe { libc::_exit(0) }
        }
    }
    // SAFETY: umask(2) has no preconditions.
    unsafe { libc::umask(0) };

    // Drop the syslog socket before the sweep closes it behind libc's back;
    // the next syslog line reopens it (C++ opens syslog after daemonising).
    if SYSLOG_OPEN.swap(false, Ordering::SeqCst) {
        // SAFETY: closelog(3) has no preconditions.
        unsafe { libc::closelog() };
    }

    // SAFETY: sysconf only reads the descriptor limit.
    let max_fd = match unsafe { libc::sysconf(libc::_SC_OPEN_MAX) } {
        value if value > 0 => value,
        _ => 1024,
    };
    for fd in 0..max_fd {
        // SAFETY: closing an unused descriptor is harmless.
        unsafe { libc::close(fd as libc::c_int) };
    }

    // SAFETY: the path is a static, NUL-terminated C string.
    let null_fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
    if null_fd != libc::STDIN_FILENO {
        if null_fd >= 0 {
            // SAFETY: null_fd is owned here.
            unsafe { libc::close(null_fd) };
        }
        return Err("cannot open /dev/null as stdin".to_owned());
    }

    let Ok(path) = std::ffi::CString::new(log_file) else {
        return Err("invalid log file name".to_owned());
    };
    let flags = if std::fs::metadata(log_file).is_ok() {
        libc::O_RDWR | libc::O_APPEND
    } else {
        libc::O_RDWR | libc::O_CREAT
    };
    // SAFETY: `path` is NUL-terminated and the mode is only used with O_CREAT.
    let log_fd = unsafe { libc::open(path.as_ptr(), flags, 0o666 as libc::c_uint) };
    if log_fd != libc::STDOUT_FILENO {
        if log_fd >= 0 {
            // SAFETY: log_fd is owned here.
            unsafe { libc::close(log_fd) };
        }
        return Err(format!("cannot open {log_file} as stdout"));
    }
    // SAFETY: dup2 duplicates the now-open stdout onto stderr.
    if unsafe { libc::dup2(libc::STDOUT_FILENO, libc::STDERR_FILENO) } != libc::STDERR_FILENO {
        return Err("dup2 failed".to_owned());
    }
    Ok(())
}

/// If the pid file names a live process running this executable, sends it
/// SIGINT (utils.cpp:283-296).
fn kill_previous_instance() {
    let Ok(contents) = std::fs::read_to_string(pid_file_path()) else {
        return;
    };
    let pid: libc::pid_t = contents
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    if pid <= 0 {
        return;
    }
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return;
    };
    let invoked = std::env::args_os()
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    if exe == invoked {
        // utils.cpp:294
        ms_log::log!("Found {} running with pid {}, killing....", APP_NAME, pid);
        // SAFETY: kill(2) only signals the pid recorded in our pid file.
        unsafe { libc::kill(pid, libc::SIGINT) };
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The real backend: local time through `localtime_r` and lines through
/// syslog when `-g` is active.
///
/// `openlog` runs lazily on the first syslog line, and again after
/// `become_daemon` closed it (C++ opens it in `main` after daemonising,
/// `minisatip.cpp:1919-1923`).
pub struct SysLogBackend {
    perror: bool,
}

impl SysLogBackend {
    /// `slog` is `opts.slog`: more than one `-g` also mirrors lines to stderr.
    pub fn new(slog: u32) -> Self {
        Self { perror: slog > 1 }
    }
}

impl Backend for SysLogBackend {
    fn timestamp(&self) -> Timestamp {
        // SAFETY: gettimeofday only writes into the provided timeval.
        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
        // SAFETY: `tv` is a valid, writable timeval.
        if unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) } != 0 {
            return Timestamp::fallback();
        }
        // SAFETY: localtime_r only reads the time_t and writes into `tm`.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `tm` is a valid, writable tm and `tv.tv_sec` is its time_t.
        if unsafe { libc::localtime_r(&tv.tv_sec, &mut tm) }.is_null() {
            return Timestamp::fallback();
        }
        Timestamp::new(
            tm.tm_mday.max(0) as u32,
            (tm.tm_mon + 1).max(0) as u32,
            tm.tm_hour.max(0) as u32,
            tm.tm_min.max(0) as u32,
            tm.tm_sec.max(0) as u32,
            u32::try_from(tv.tv_usec / 1000).unwrap_or(0),
        )
    }

    fn syslog(&self, line: &str) {
        if !SYSLOG_OPEN.swap(true, Ordering::SeqCst) {
            let options = libc::LOG_NDELAY
                | libc::LOG_NOWAIT
                | libc::LOG_PID
                | if self.perror { libc::LOG_PERROR } else { 0 };
            // SAFETY: the identifier is a static NUL-terminated C string;
            // openlog keeps the pointer, which stays valid for the process.
            unsafe { libc::openlog(c"minisatip".as_ptr(), options, libc::LOG_DAEMON) };
        }
        // C++ passes the line to "%s", which stops at a NUL; strip any so the
        // whole line reaches syslog.
        let clean: String = line.chars().filter(|c| *c != '\0').collect();
        let Ok(message) = CString::new(clean) else {
            return;
        };
        // SAFETY: "%s" is a static NUL-terminated format string and `message`
        // is NUL-terminated; syslog copies both before returning.
        unsafe { libc::syslog(libc::LOG_NOTICE, c"%s".as_ptr(), message.as_ptr()) };
    }
}

/// Writes a diagnostic through the logger (`LOG0` for runtime failures).
pub fn log_error(args: std::fmt::Arguments<'_>) {
    ms_log::log0!("{args}");
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
