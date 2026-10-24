//! Signal handling. The only unsafe code in the binary lives here.

#![allow(unsafe_code)]

use std::ffi::CString;
use std::io::Write as _;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use ms_log::{Backend, Timestamp};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Mirrors libc's process-wide syslog connection state, so `become_daemon`
/// can close it before its descriptor sweep and the next line reopens it
/// with the C++ options (`minisatip.cpp:1919-1923`).
static SYSLOG_OPEN: AtomicBool = AtomicBool::new(false);

/// glibc's default syslog socket (`_PATH_LOG`).
#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
const SYSLOG_PATH: &str = "/dev/log";

/// `LOG_DAEMON | LOG_NOTICE`, the PRI the C++ `syslog(LOG_NOTICE, ...)` emits.
#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
const SYSLOG_PRI: u8 = (libc::LOG_DAEMON | libc::LOG_NOTICE) as u8;

/// musl's `syslog` uses UTC; this keeps the socket glibc would keep open.
#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
static SYSLOG_SOCKET: Mutex<Option<UnixDatagram>> = Mutex::new(None);

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
        #[cfg(target_env = "musl")]
        {
            syslog_close(&SYSLOG_SOCKET);
        }
        #[cfg(not(target_env = "musl"))]
        {
            // SAFETY: closelog(3) has no preconditions.
            unsafe { libc::closelog() };
        }
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

    redirect_to_log_file(log_file)
}

/// Opens `path` and duplicates it onto stdout and stderr (`becomeDaemon`,
/// utils.cpp:315-340).
pub fn redirect_to_log_file(path: &str) -> Result<(), String> {
    let Ok(cpath) = CString::new(path) else {
        return Err("invalid log file name".to_owned());
    };
    let flags = if std::fs::metadata(path).is_ok() {
        libc::O_RDWR | libc::O_APPEND
    } else {
        libc::O_RDWR | libc::O_CREAT
    };
    // SAFETY: `cpath` is NUL-terminated and the mode is only used with O_CREAT.
    let log_fd = unsafe { libc::open(cpath.as_ptr(), flags, 0o666 as libc::c_uint) };
    if log_fd < 0 {
        return Err(format!("cannot open {path} as stdout"));
    }
    if log_fd != libc::STDOUT_FILENO {
        // SAFETY: log_fd is owned here and dup2 leaves it open.
        let duplicated = unsafe { libc::dup2(log_fd, libc::STDOUT_FILENO) };
        // SAFETY: log_fd is still owned here.
        unsafe { libc::close(log_fd) };
        if duplicated != libc::STDOUT_FILENO {
            return Err("dup2 failed".to_owned());
        }
    }
    // SAFETY: dup2 duplicates the now-open stdout onto stderr.
    if unsafe { libc::dup2(libc::STDOUT_FILENO, libc::STDERR_FILENO) } != libc::STDERR_FILENO {
        return Err("dup2 failed".to_owned());
    }
    Ok(())
}

/// Whether stderr is a different open file than stdout (`fstat` comparison).
pub fn stderr_differs_from_stdout() -> bool {
    // SAFETY: fstat writes only into the stack `stat` values.
    unsafe {
        let mut out: libc::stat = std::mem::zeroed();
        let mut err: libc::stat = std::mem::zeroed();
        if libc::fstat(libc::STDOUT_FILENO, &mut out) != 0
            || libc::fstat(libc::STDERR_FILENO, &mut err) != 0
        {
            return true;
        }
        out.st_dev != err.st_dev || out.st_ino != err.st_ino
    }
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

/// Where `-g` lines go: the C library on glibc, a `/dev/log` datagram on musl.
enum SyslogTarget {
    #[cfg_attr(target_env = "musl", allow(dead_code))]
    Libc,
    Datagram(PathBuf),
}

/// The real backend: local time through `localtime_r`, lines through syslog
/// when `-g` is active, and `--log-max-size` rotation for the `-F` file.
///
/// On musl, lines go straight to /dev/log; otherwise `openlog` runs lazily,
/// and again after `become_daemon` closed it (minisatip.cpp:1919-1923).
pub struct SysLogBackend {
    perror: bool,
    target: SyslogTarget,
    rotation: LogRotation,
}

impl SysLogBackend {
    /// `slog` is `opts.slog`: more than one `-g` also mirrors lines to stderr.
    pub fn new(slog: u32) -> Self {
        #[cfg(target_env = "musl")]
        let target = SyslogTarget::Datagram(PathBuf::from(SYSLOG_PATH));
        #[cfg(not(target_env = "musl"))]
        let target = SyslogTarget::Libc;
        Self {
            perror: slog > 1,
            target,
            rotation: LogRotation::default(),
        }
    }

    /// A datagram writer at `path`, so tests can run on every unix target.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_socket_path(slog: u32, path: impl Into<PathBuf>) -> Self {
        Self {
            perror: slog > 1,
            target: SyslogTarget::Datagram(path.into()),
            rotation: LogRotation::default(),
        }
    }

    /// Handle used by `main` to arm rotation after the `-F` redirect.
    pub fn rotation(&self) -> LogRotation {
        self.rotation.clone()
    }

    /// Datagram output used on musl and by tests.
    #[cfg_attr(not(target_env = "musl"), allow(dead_code))]
    fn syslog_datagram(&self, path: &Path, line: &str) {
        SYSLOG_OPEN.store(true, Ordering::SeqCst);
        let timestamp = local_timestamp();
        let text = syslog_line(
            SYSLOG_PRI,
            timestamp.month,
            timestamp.day,
            timestamp.hour,
            timestamp.minute,
            timestamp.second,
            std::process::id(),
            line,
        );
        if self.perror {
            // glibc's LOG_PERROR ignores write errors and adds a missing newline.
            let message: String = line.chars().filter(|c| *c != '\0').collect();
            let end = if message.ends_with('\n') { "" } else { "\n" };
            let _ = write!(
                std::io::stderr().lock(),
                "{APP_NAME}[{}]: {message}{end}",
                std::process::id()
            );
        }
        syslog_send(path, &SYSLOG_SOCKET, text.as_bytes());
    }

    /// Production glibc output through the C library.
    #[cfg_attr(target_env = "musl", allow(dead_code))]
    fn syslog_libc(&self, line: &str) {
        if !SYSLOG_OPEN.swap(true, Ordering::SeqCst) {
            let options = libc::LOG_NDELAY
                | libc::LOG_NOWAIT
                | libc::LOG_PID
                | if self.perror { libc::LOG_PERROR } else { 0 };
            // SAFETY: the identifier is a static NUL-terminated C string;
            // openlog keeps the pointer, which stays valid for the process.
            unsafe { libc::openlog(c"minisatip".as_ptr(), options, libc::LOG_DAEMON) };
        }
        let clean: String = line.chars().filter(|c| *c != '\0').collect();
        let Ok(message) = CString::new(clean) else {
            return;
        };
        // SAFETY: "%s" is a static NUL-terminated format string and `message`
        // is NUL-terminated; syslog copies both before returning.
        unsafe { libc::syslog(libc::LOG_NOTICE, c"%s".as_ptr(), message.as_ptr()) };
    }
}

/// Rotation state for `--log-max-size`, shared with the logger backend.
#[derive(Clone, Default)]
pub struct LogRotation(Arc<Mutex<Option<Rotation>>>);

struct Rotation {
    path: PathBuf,
    limit: u64,
    written: u64,
}

impl LogRotation {
    /// Starts counting at the current size of `path` (after the `-F` redirect).
    pub fn start(&self, path: &str, limit: u64) {
        let written = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(Rotation {
            path: PathBuf::from(path),
            limit,
            written,
        });
    }

    /// Rotates before `line` when it would pass the limit, then counts it.
    ///
    /// Never blocks: a panic can arrive while the logger holds this lock, and
    /// the hook must not deadlock. If the lock is busy the line is written
    /// without being counted (and so not rotated).
    fn before_line(&self, line: &str) {
        let mut state = match self.0.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let Some(rotation) = state.as_mut() else {
            return;
        };
        let len = line.len() as u64 + 1;
        if rotation.limit > 0 && rotation.written > 0 && rotation.written + len > rotation.limit {
            match rotate_log_file(&rotation.path) {
                Ok(()) => rotation.written = 0,
                Err(error) => {
                    // Stop retrying so a failing rotate emits one line, not one per write.
                    rotation.limit = 0;
                    write_error_line(&format!("log rotation failed: {error}"));
                }
            }
        }
        rotation.written += len;
    }
}

/// `rename(path, path.1)`, then a fresh `path` on fds 1 and 2.
fn rotate_log_file(path: &Path) -> Result<(), String> {
    let backup = PathBuf::from(format!("{}.1", path.display()));
    std::fs::rename(path, &backup)
        .map_err(|error| format!("rename {} to {}: {error}", path.display(), backup.display()))?;
    let Ok(cpath) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        let _ = std::fs::rename(&backup, path);
        return Err("invalid log file name".to_owned());
    };
    // SAFETY: `cpath` is NUL-terminated and the mode is only used with O_CREAT.
    let fd = unsafe {
        libc::open(
            cpath.as_ptr(),
            libc::O_RDWR | libc::O_CREAT,
            0o666 as libc::c_uint,
        )
    };
    if fd < 0 {
        let _ = std::fs::rename(&backup, path);
        return Err(format!(
            "cannot open {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: dup2 duplicates the freshly opened file onto stdout and stderr.
    let stdout_duplicated = unsafe { libc::dup2(fd, libc::STDOUT_FILENO) } == libc::STDOUT_FILENO;
    if !stdout_duplicated {
        if fd != libc::STDOUT_FILENO {
            // SAFETY: fd is owned here and dup2 left the target unchanged.
            unsafe { libc::close(fd) };
        }
        let _ = std::fs::rename(&backup, path);
        return Err("dup2 failed".to_owned());
    }
    // SAFETY: fd is still owned here; stdout now names the fresh file.
    let stderr_duplicated = unsafe { libc::dup2(fd, libc::STDERR_FILENO) } == libc::STDERR_FILENO;
    if fd != libc::STDOUT_FILENO {
        // SAFETY: fd is owned here and the duplicates keep the file open.
        unsafe { libc::close(fd) };
    }
    if !stderr_duplicated {
        // fd 1 already points at the fresh file, so renaming the backup back
        // would leave stdout on an unlinked path. Keep it there and report.
        return Err("dup2 failed".to_owned());
    }
    Ok(())
}

/// Writes a rotation failure straight to fd 2; the logger mutex is held here.
fn write_error_line(message: &str) {
    let text = format!("{message}\n");
    // SAFETY: write(2) only reads `text` and writes to the stderr descriptor.
    unsafe {
        let _ = libc::write(libc::STDERR_FILENO, text.as_ptr().cast(), text.len());
    }
}

/// `gettimeofday` + `localtime_r`, as every C++ log line is stamped.
pub fn local_timestamp() -> Timestamp {
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

/// Formats one line as glibc's `syslog` does:
/// `<PRI>Mmm dd HH:MM:SS minisatip[PID]: MESSAGE`.
#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
fn syslog_line(
    pri: u8,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    pid: u32,
    message: &str,
) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS[(month.clamp(1, 12) - 1) as usize];
    let message: String = message.chars().filter(|c| *c != '\0').collect();
    format!("<{pri}>{month} {day:>2} {hour:02}:{min:02}:{sec:02} {APP_NAME}[{pid}]: {message}")
}

/// Sends one datagram, connecting lazily and reconnecting once on failure.
///
/// Never blocks: a panic can arrive while the logger holds this lock, so a
/// busy lock falls back to a one-shot datagram (the line is never dropped).
#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
fn syslog_send(path: &Path, state: &Mutex<Option<UnixDatagram>>, line: &[u8]) {
    let mut socket = match state.try_lock() {
        Ok(socket) => socket,
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        Err(TryLockError::WouldBlock) => {
            if let Some(socket) = syslog_connect(path) {
                let _ = socket.send(line);
            }
            return;
        }
    };
    if socket.is_none() {
        *socket = syslog_connect(path);
    }
    if socket.as_ref().is_some_and(|s| s.send(line).is_ok()) {
        return;
    }
    // glibc reconnects once; without LOG_CONS a second failure drops the line.
    *socket = syslog_connect(path);
    if let Some(socket) = socket.as_ref() {
        let _ = socket.send(line);
    }
}

#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
fn syslog_connect(path: &Path) -> Option<UnixDatagram> {
    let socket = UnixDatagram::unbound().ok()?;
    socket.connect(path).ok()?;
    Some(socket)
}

#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
fn syslog_close(state: &Mutex<Option<UnixDatagram>>) {
    *state.lock().unwrap_or_else(|error| error.into_inner()) = None;
}

impl Backend for SysLogBackend {
    fn timestamp(&self) -> Timestamp {
        local_timestamp()
    }

    fn syslog(&self, line: &str) {
        // C++ passes the line to "%s", which stops at a NUL; both sinks strip any.
        match &self.target {
            SyslogTarget::Datagram(path) => self.syslog_datagram(path, line),
            SyslogTarget::Libc => self.syslog_libc(line),
        }
    }

    fn stdout(&self, line: &str) {
        self.rotation.before_line(line);
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
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
    use super::{SYSLOG_PRI, elapsed_string, syslog_line, syslog_send};
    use std::os::unix::net::UnixDatagram;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn elapsed_matches_cpp_format() {
        // minisatip.cpp:1549-1552: "%.0d%s%02d:%02d:%02d".
        assert_eq!(elapsed_string(0), "00:00:00");
        assert_eq!(elapsed_string(61), "00:01:01");
        assert_eq!(elapsed_string(3 * 86_400 + 3_661), "3d 01:01:01");
    }

    #[test]
    fn syslog_line_pads_like_glibc() {
        assert_eq!(SYSLOG_PRI, 29);
        assert_eq!(
            syslog_line(SYSLOG_PRI, 10, 4, 9, 5, 7, 1234, "hello"),
            "<29>Oct  4 09:05:07 minisatip[1234]: hello"
        );
        assert_eq!(
            syslog_line(SYSLOG_PRI, 10, 24, 2, 5, 41, 1, "hello"),
            "<29>Oct 24 02:05:41 minisatip[1]: hello"
        );
    }

    #[test]
    fn syslog_line_names_every_month_and_strips_nul() {
        let names = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        for (index, name) in names.iter().enumerate() {
            let line = syslog_line(SYSLOG_PRI, index as u32 + 1, 1, 0, 0, 0, 1, "m");
            assert!(line.starts_with(&format!("<29>{name} ")), "{line}");
        }
        assert_eq!(
            syslog_line(SYSLOG_PRI, 1, 1, 0, 0, 0, 1, "a\0b\0"),
            "<29>Jan  1 00:00:00 minisatip[1]: ab"
        );
    }

    /// Empty socket directory and listening path for one test.
    fn temp_socket(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("minisatip-syslog-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        (dir, path)
    }

    fn receiver(path: &Path) -> UnixDatagram {
        let socket = UnixDatagram::bind(path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
    }

    #[test]
    fn syslog_writer_sends_one_datagram() {
        let (dir, path) = temp_socket("one");
        let listener = receiver(&path);
        let state = Mutex::new(None);
        let line = b"<29>Oct  4 09:05:07 minisatip[42]: hello";
        syslog_send(&path, &state, line);
        let mut buffer = [0u8; 128];
        let len = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..len], line);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn syslog_writer_reconnects_after_rebind() {
        let (dir, path) = temp_socket("rebind");
        let state = Mutex::new(None);
        let listener = receiver(&path);
        syslog_send(&path, &state, b"first");
        let mut buffer = [0u8; 128];
        let len = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..len], b"first");
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        let listener = receiver(&path);
        syslog_send(&path, &state, b"second");
        let len = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..len], b"second");
        let _ = std::fs::remove_dir_all(dir);
    }
}
