//! minisatip logging core, ported from the C++ `utils/logging/logging.cpp`.
//!
//! The crate owns one process-wide logger, [`LOGGER`], installed once by
//! [`init`]. That [`OnceLock`] is the only global: everything below reads the
//! configuration from it, and callers must set it before the first log line
//! they care about. Before [`init`] runs the logger behaves like the C++ binary
//! during option parsing: the mask is [`LOG_GENERAL`], debugging is off, lines
//! go to stdout and no source location is printed. A statically initialised
//! default logger is used rather than `OnceLock::get_or_init`, so a later
//! [`init`] still wins.
//!
//! # Modules and masks
//!
//! [`LOG_LEVELS`] is the `loglevels[]` table (`utils.h:207-211`); bit N of a
//! mask selects module N (`logging.h:27-45`). [`module_mask`] is the C++
//! `log_module_mask` (`minisatip.cpp:288-297`).
//!
//! # Per-module default mask
//!
//! The C++ sources select a per-file default with `#define DEFAULT_LOG ...`.
//! The port keeps that convention: a module using [`logm!`], [`debugm!`],
//! [`hexdump!`] or [`dump_packets!`] defines
//!
//! ```
//! const DEFAULT_LOG: u32 = ms_log::LOG_GENERAL;
//! ```
//!
//! in its own module. The macros resolve `DEFAULT_LOG` at the call site.
//!
//! # Thread names
//!
//! [`set_thread_name`] replaces C++ `thread_info[].thread_name`; the name is
//! thread-local. Threads that never call it fall back to the standard thread
//! name (`"main"` for the main thread, empty when the thread is unnamed, as the
//! C++ empty `thread_name`).
//!
//! # Deviations from the C++
//!
//! * The C++ `thread_info[].last_log` crash-dump bookkeeping is not ported.
//! * [`Timestamp`] can omit milliseconds so the C++ fallback
//!   `"01/01 00:00:20"` (`logging.cpp:110-114`) is reproduced exactly.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Write as _};
use std::sync::{Mutex, OnceLock, PoisonError, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};

mod hexdump;

pub use hexdump::{crc_32, dump_packets, hexdump};

/// `LOG_GENERAL` (`logging.h:27`).
pub const LOG_GENERAL: u32 = 1;
/// `LOG_HTTP` (`logging.h:28`).
pub const LOG_HTTP: u32 = 1 << 1;
/// `LOG_SOCKETWORKS` (`logging.h:29`).
pub const LOG_SOCKETWORKS: u32 = 1 << 2;
/// `LOG_STREAM` (`logging.h:30`).
pub const LOG_STREAM: u32 = 1 << 3;
/// `LOG_ADAPTER` (`logging.h:31`).
pub const LOG_ADAPTER: u32 = 1 << 4;
/// `LOG_SATIPC` (`logging.h:32`).
pub const LOG_SATIPC: u32 = 1 << 5;
/// `LOG_PMT` (`logging.h:33`).
pub const LOG_PMT: u32 = 1 << 6;
/// `LOG_TABLES` (`logging.h:34`).
pub const LOG_TABLES: u32 = 1 << 7;
/// `LOG_DVBAPI` (`logging.h:35`).
pub const LOG_DVBAPI: u32 = 1 << 8;
/// `LOG_LOCK` (`logging.h:36`).
pub const LOG_LOCK: u32 = 1 << 9;
/// `LOG_NETCEIVER` (`logging.h:37`).
pub const LOG_NETCEIVER: u32 = 1 << 10;
/// `LOG_DVBCA`, the `-l ca` module (`logging.h:38`).
pub const LOG_DVBCA: u32 = 1 << 11;
/// `LOG_SOCKET` (`logging.h:40`).
pub const LOG_SOCKET: u32 = 1 << 12;
/// `LOG_UTILS` (`logging.h:41`).
pub const LOG_UTILS: u32 = 1 << 13;
/// `LOG_DMX` (`logging.h:42`).
pub const LOG_DMX: u32 = 1 << 14;
/// `LOG_SSDP` (`logging.h:43`).
pub const LOG_SSDP: u32 = 1 << 15;
/// `LOG_DVB` (`logging.h:44`).
pub const LOG_DVB: u32 = 1 << 16;
/// `LOG_DDCI` (`logging.h:45`).
pub const LOG_DDCI: u32 = 1 << 17;

/// Module names in bit order (`loglevels[]`, `utils.h:207-211`).
pub const LOG_LEVELS: [&str; 18] = [
    "general",
    "http",
    "socketworks",
    "stream",
    "adapter",
    "satipc",
    "pmt",
    "tables",
    "dvbapi",
    "lock",
    "netceiver",
    "ca",
    "socket",
    "utils",
    "dmx",
    "ssdp",
    "dvb",
    "ddci",
];

/// Mask selecting every module (`-l all`).
pub const ALL_LOG_MODULES: u32 = (1 << LOG_LEVELS.len()) - 1;

/// Bit mask for a `-l`/`-v` module name, `all` for every module, 0 if unknown
/// (`log_module_mask`, `minisatip.cpp:288-297`).
pub fn module_mask(name: &str) -> u32 {
    let mut all = 0;
    for (index, module) in LOG_LEVELS.iter().enumerate() {
        if name == *module {
            return 1u32 << index;
        }
        all |= 1u32 << index;
    }
    if name == "all" { all } else { 0 }
}

/// The `errno` C++ would print for `err`: its OS code, or 0 when it has none.
pub fn errno(err: &io::Error) -> i32 {
    err.raw_os_error().unwrap_or(0)
}

/// The text C++ `strerror(errno)` prints for `err`.
///
/// Rust's [`io::Error`] Display appends `" (os error N)"` to errors created
/// from an OS code, while C++ prints only the platform message. This returns
/// Display with that suffix removed; an error without an OS code keeps its
/// Display text unchanged.
pub fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    let Some(code) = err.raw_os_error() else {
        return text;
    };
    let suffix = format!(" (os error {code})");
    text.strip_suffix(&suffix)
        .map(str::to_owned)
        .unwrap_or(text)
}

/// Local-time fields returned by a [`Backend`].
///
/// `millis` is `None` only for the C++ fallback timestamp, whose text is
/// `"01/01 00:00:20"` without a fractional part (`logging.cpp:110-114`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// Day of month.
    pub day: u32,
    /// Month, 1-12.
    pub month: u32,
    /// Hour, 0-23.
    pub hour: u32,
    /// Minute, 0-59.
    pub minute: u32,
    /// Second, 0-59.
    pub second: u32,
    /// Milliseconds, or `None` for the fallback timestamp.
    pub millis: Option<u32>,
}

impl Timestamp {
    /// A timestamp including milliseconds.
    pub const fn new(
        day: u32,
        month: u32,
        hour: u32,
        minute: u32,
        second: u32,
        millis: u32,
    ) -> Self {
        Self {
            day,
            month,
            hour,
            minute,
            second,
            millis: Some(millis),
        }
    }

    /// `"01/01 00:00:20"`, returned when `gettimeofday`/`localtime` fail.
    pub const fn fallback() -> Self {
        Self {
            day: 1,
            month: 1,
            hour: 0,
            minute: 0,
            second: 20,
            millis: None,
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}/{:02} {:02}:{:02}:{:02}",
            self.day, self.month, self.hour, self.minute, self.second
        )?;
        if let Some(millis) = self.millis {
            write!(f, ".{millis:03}")?;
        }
        Ok(())
    }
}

/// Where log lines and timestamps come from.
///
/// The binary supplies a syslog/localtime implementation; tests supply fakes.
pub trait Backend: Send + Sync {
    /// Current local time (`get_current_timestamp_log`, `logging.cpp:105-119`).
    fn timestamp(&self) -> Timestamp;

    /// Emits one already-formatted line through `syslog(LOG_NOTICE, ...)`.
    fn syslog(&self, line: &str);

    /// Emits one already-formatted line to stdout, flushed per line.
    fn stdout(&self, line: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// Logger configuration installed by [`init`].
pub struct Config {
    /// `opts.log`: modules logging at the normal level.
    pub log: u32,
    /// `opts.debug`: modules logging at the debug level.
    pub debug: u32,
    /// `opts.slog`: non-zero sends lines to syslog instead of stdout.
    pub slog: u32,
    /// `opts.file_line`: print `file:line` in the prefix.
    pub file_line: bool,
    /// Timestamp and output implementation.
    pub backend: Box<dyn Backend>,
}

enum BackendSlot {
    Default,
    Custom(Box<dyn Backend>),
}

/// The C++ `output[2][2000]`, `idx` and `times` statics (`logging.cpp:38-40`).
struct State {
    output: [Vec<u8>; 2],
    idx: usize,
    times: u32,
}

impl State {
    const fn new() -> Self {
        Self {
            output: [Vec::new(), Vec::new()],
            idx: 0,
            times: 0,
        }
    }
}

/// A logger instance. The process uses [`LOGGER`]; tests may build their own
/// with [`Logger::new`].
pub struct Logger {
    log: u32,
    debug: u32,
    slog: u32,
    file_line: bool,
    backend: BackendSlot,
    state: Mutex<State>,
}

/// C++ `output[2][2000]` holds at most 1999 bytes plus the terminating NUL.
const MAX_LINE_BYTES: usize = 1999;

impl Logger {
    /// Builds a logger from `config`.
    pub fn new(config: Config) -> Self {
        Self {
            log: config.log,
            debug: config.debug,
            slog: config.slog,
            file_line: config.file_line,
            backend: BackendSlot::Custom(config.backend),
            state: Mutex::new(State::new()),
        }
    }

    fn backend(&self) -> &dyn Backend {
        match &self.backend {
            BackendSlot::Default => &DEFAULT_BACKEND,
            BackendSlot::Custom(backend) => backend.as_ref(),
        }
    }

    /// Whether `mask` is enabled for normal logging.
    pub fn enabled(&self, mask: u32) -> bool {
        self.log & mask != 0
    }

    /// Whether `mask` is enabled for debug logging.
    pub fn debug_enabled(&self, mask: u32) -> bool {
        self.debug & mask != 0
    }

    /// Port of `_log` (`logging.cpp:35-103`).
    ///
    /// `file` and `line` are ignored unless `file_line` is set, exactly like
    /// the C++ prefix.
    pub fn write(&self, file: &str, line: u32, args: fmt::Arguments<'_>) {
        // Format before taking the lock, so a Display impl that logs cannot
        // deadlock on the logger mutex.
        let message = args.to_string();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // C++ alternates the buffers: `idx = 1 - idx` (`logging.cpp:54`).
        let idx = 1 - state.idx;
        state.idx = idx;

        let prefix = self.prefix(file, line);
        let mut text = prefix.clone();
        text.push_str(&message);
        let text = truncate_bytes(&text, MAX_LINE_BYTES);

        // C++ compares the text after the prefix, using the current prefix
        // length as the offset into both buffers (`logging.cpp:78`).
        let prefix_len = prefix.len().min(MAX_LINE_BYTES);
        let current = text.get(prefix_len..).unwrap_or("").as_bytes();
        let other = state.output[1 - idx].get(prefix_len..).unwrap_or_default();
        let same = current == other;

        if same {
            state.times += 1;
        } else {
            if state.times > 0 {
                self.emit(&format!("Message repeated {} times", state.times));
            }
            state.times = 0;
        }

        state.output[idx] = text.as_bytes().to_vec();
        if state.times == 0 {
            self.emit(text);
        }
    }

    /// Emits a panic line: like [`Logger::write`], but without the repeat
    /// suppression and never blocking on the state lock.
    pub fn write_panic(&self, message: &str) {
        let text = if self.slog != 0 {
            truncate_bytes(message, MAX_LINE_BYTES).to_owned()
        } else {
            let timestamp = self.backend().timestamp();
            let thread = thread_name();
            truncate_bytes(
                &format!("[{timestamp} {thread}]: {message}"),
                MAX_LINE_BYTES,
            )
            .to_owned()
        };
        match self.state.try_lock() {
            // The panic did not hold the state lock: flush a pending repeat
            // line, as the next normal line would, then emit the panic.
            Ok(mut state) => {
                self.flush_repeats(&mut state);
                self.emit(&text);
            }
            Err(TryLockError::Poisoned(error)) => {
                let mut state = error.into_inner();
                self.flush_repeats(&mut state);
                self.emit(&text);
            }
            // The panicking thread holds the lock: bypass the logger state.
            Err(TryLockError::WouldBlock) => {
                let backend = self.backend();
                if self.slog != 0 {
                    backend.syslog(&text);
                } else {
                    backend.stdout(&text);
                }
            }
        }
    }

    /// Emits `Message repeated N times` and resets the counter.
    fn flush_repeats(&self, state: &mut State) {
        if state.times > 0 {
            self.emit(&format!("Message repeated {} times", state.times));
            state.times = 0;
        }
    }

    fn prefix(&self, file: &str, line: u32) -> String {
        // `opts.slog` suppresses the timestamp, thread and source entirely.
        if self.slog != 0 {
            return String::new();
        }
        let timestamp = self.backend().timestamp();
        let thread = thread_name();
        if self.file_line {
            format!("[{timestamp} {thread}] {file}:{line}: ")
        } else {
            format!("[{timestamp} {thread}]: ")
        }
    }

    fn emit(&self, line: &str) {
        let backend = self.backend();
        if self.slog != 0 {
            backend.syslog(line);
        } else {
            backend.stdout(line);
        }
    }
}

/// Cuts `text` to at most `max` bytes, backing up to a char boundary.
fn truncate_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Const-initialised default logger. It is a `static` (not a `const`) so
/// [`logger`] can hand out a `'static` reference; unlike `get_or_init` it does
/// not occupy the [`OnceLock`], so a later [`init`] still takes effect.
static DEFAULT_LOGGER: Logger = Logger {
    log: LOG_GENERAL,
    debug: 0,
    slog: 0,
    file_line: false,
    backend: BackendSlot::Default,
    state: Mutex::new(State::new()),
};

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Local-time clock for lines logged before [`init`], supplied by the binary.
static PREINIT_CLOCK: OnceLock<fn() -> Timestamp> = OnceLock::new();

/// Installs the clock for lines logged before [`init`] (C++ uses `localtime`
/// for every line); only the first call has an effect.
pub fn set_preinit_clock(clock: fn() -> Timestamp) {
    let _ = PREINIT_CLOCK.set(clock);
}

/// Timestamp of a line logged before [`init`]: the installed clock, else UTC.
pub fn preinit_timestamp() -> Timestamp {
    PREINIT_CLOCK.get().map_or_else(utc_now, |clock| clock())
}

/// Installs the process-wide logger. Only the first call has an effect.
pub fn init(config: Config) {
    let _ = LOGGER.set(Logger::new(config));
}

/// The process-wide logger, or the const default before [`init`].
pub fn logger() -> &'static Logger {
    LOGGER.get().unwrap_or(&DEFAULT_LOGGER)
}

/// Whether `mask` is enabled in the process-wide logger.
pub fn enabled(mask: u32) -> bool {
    logger().enabled(mask)
}

/// Whether `mask` is enabled for debug output in the process-wide logger.
pub fn debug_enabled(mask: u32) -> bool {
    logger().debug_enabled(mask)
}

/// Port of `_log` for the process-wide logger.
pub fn write(file: &str, line: u32, args: fmt::Arguments<'_>) {
    logger().write(file, line, args);
}

/// Emits a panic line through the process-wide logger; never panics.
pub fn write_panic(message: &str) {
    logger().write_panic(message);
}

/// The process-wide logger's `opts.slog` value (`-g` count).
pub fn slog() -> u32 {
    logger().slog
}

thread_local! {
    static THREAD_NAME: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Sets this thread's name for log prefixes (C++ `thread_info[].thread_name`).
pub fn set_thread_name(name: &str) {
    let _ = THREAD_NAME.try_with(|slot| *slot.borrow_mut() = Some(name.to_owned()));
}

/// This thread's log name; the standard thread name when never set.
///
/// Uses `try_with` so logging from a TLS destructor does not panic.
pub fn thread_name() -> String {
    THREAD_NAME
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| std::thread::current().name().unwrap_or("").to_owned())
}

/// Backend used before [`init`] runs.
const DEFAULT_BACKEND: DefaultBackend = DefaultBackend;

/// Pre-init backend: [`preinit_timestamp`], stdout output.
struct DefaultBackend;

impl Backend for DefaultBackend {
    fn timestamp(&self) -> Timestamp {
        preinit_timestamp()
    }

    fn syslog(&self, line: &str) {
        self.stdout(line);
    }
}

/// Reads the system clock as UTC without libc (`localtime` needs a backend).
fn utc_now() -> Timestamp {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let (_, month, day) = civil_from_days(days);
    let second_of_day = secs % 86_400;
    Timestamp::new(
        day,
        month,
        u32::try_from(second_of_day / 3_600).unwrap_or(0),
        u32::try_from(second_of_day % 3_600 / 60).unwrap_or(0),
        u32::try_from(second_of_day % 60).unwrap_or(0),
        now.subsec_millis(),
    )
}

/// Days since 1970-01-01 to a Gregorian date, as in `gmtime` (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u32::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month, day)
}

/// `LOGL(level, ...)`: formats only when `mask` is in `opts.log`.
#[macro_export]
macro_rules! logl {
    ($mask:expr, $($arg:tt)*) => {{
        if $crate::enabled($mask) {
            $crate::write(file!(), line!(), format_args!($($arg)*));
        }
    }};
}

/// `LOG(...)`: general module, normal level.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::logl!($crate::LOG_GENERAL, $($arg)*)
    };
}

/// `LOGM(...)`: the calling module's `DEFAULT_LOG`, normal level.
#[macro_export]
macro_rules! logm {
    ($($arg:tt)*) => {{
        if $crate::enabled(DEFAULT_LOG) {
            $crate::write(file!(), line!(), format_args!($($arg)*));
        }
    }};
}

/// `DEBUGL(level, ...)`: formats only when `mask` is in `opts.debug`.
#[macro_export]
macro_rules! debugl {
    ($mask:expr, $($arg:tt)*) => {{
        if $crate::debug_enabled($mask) {
            $crate::write(file!(), line!(), format_args!($($arg)*));
        }
    }};
}

/// `DEBUGM(...)`: the calling module's `DEFAULT_LOG`, debug level.
#[macro_export]
macro_rules! debugm {
    ($($arg:tt)*) => {{
        if $crate::debug_enabled(DEFAULT_LOG) {
            $crate::write(file!(), line!(), format_args!($($arg)*));
        }
    }};
}

/// `LOG0(...)`: always logs.
#[macro_export]
macro_rules! log0 {
    ($($arg:tt)*) => {{
        $crate::write(file!(), line!(), format_args!($($arg)*));
    }};
}

/// `LOG_AND_RETURN(rc, ...)`: logs and returns `rc`.
#[macro_export]
macro_rules! log_and_return {
    ($rc:expr, $($arg:tt)*) => {{
        $crate::log!($($arg)*);
        return $rc;
    }};
}

/// `hexdump(desc, bytes)` gated on the module's debug mask (`utils.h:172-174`).
#[macro_export]
macro_rules! hexdump {
    ($desc:expr, $data:expr) => {{
        if $crate::debug_enabled(DEFAULT_LOG) {
            $crate::hexdump(::core::option::Option::Some($desc), $data);
        }
    }};
}

/// `dump_packets(msg, bytes, offset)` gated on the module's debug mask
/// (`utils.h:169-171`).
#[macro_export]
macro_rules! dump_packets {
    ($msg:expr, $data:expr, $offset:expr) => {{
        if $crate::debug_enabled(DEFAULT_LOG) {
            $crate::dump_packets($msg, $data, $offset);
        }
    }};
}

#[cfg(test)]
mod tests;
