//! Binary entry point: wiring, logging and signal handling.

mod ca;
mod ddci;
mod dvbca;
mod hw;
mod runtime;
mod sys;

fn main() {
    // Lines logged while parsing use local time too.
    ms_log::set_preinit_clock(sys::local_timestamp);
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string().unwrap_or_else(|_| {
                eprintln!("minisatip: arguments must be valid UTF-8");
                std::process::exit(1);
            })
        })
        .collect();
    let ms_config::ParseOutcome {
        options,
        warnings,
        error,
    } = ms_config::parse_args_on(
        &args,
        ms_config::Host {
            enigma: std::path::Path::new("/etc/enigma2/settings").exists(),
            fbc: fbc_host(),
        },
    );
    // glibc getopt prints these as it walks argv; the C++ switch has no default
    // case, so they are diagnostics and parsing continues.
    for warning in &warnings {
        eprintln!("minisatip: {warning}");
    }
    let mut opts = options;
    // minisatip.cpp:1907-1910: the main thread is named before set_options.
    // The logger is installed right after parsing and before daemonising
    // (minisatip.cpp:1911-1923).
    ms_log::set_thread_name("main");
    let backend = sys::SysLogBackend::new(opts.slog);
    let rotation = backend.rotation();
    ms_log::init(ms_log::Config {
        log: opts.log,
        debug: opts.debug,
        slog: opts.slog,
        file_line: opts.file_line != 0,
        backend: Box::new(backend),
    });
    if let Some(err) = error {
        // C++ FAIL: LOG0, unlink(pid_file), exit(1).
        ms_log::log0!("{err}");
        sys::remove_pid_file();
        // usage() is printed only on the paths where the C++ code calls it
        // before exit(1) (invalid --sdt-charset, --help).
        if matches!(
            err,
            ms_config::OptError::InvalidValue {
                show_usage: true,
                ..
            }
        ) {
            print!("{}", ms_config::usage_text());
        }
        std::process::exit(1);
    }
    let program = std::env::args_os().next().unwrap_or_default();
    opts.command_line = command_line(&program.to_string_lossy(), &args);
    if opts.version {
        // print_version(0, 0); exit(0)
        println!("{}", ms_config::VERSION_BANNER);
        return;
    }
    if opts.help {
        // usage() prints the banner and help text, then exits with status 1.
        print!("{}", ms_config::usage_text());
        std::process::exit(1);
    }
    // minisatip.cpp:1914-1932: daemonise by default (opts.daemon is 1 unless
    // -f is given), then log the startup lines in C++ order. becomeDaemon's
    // result is ignored.
    if opts.daemon != 0 {
        let daemonised = sys::become_daemon(&opts.log_file);
        if daemonised.is_ok() && opts.slog == 0 && opts.log_max_size > 0 {
            // stdout is the -F file now; count from its current size.
            rotation.start(&opts.log_file, opts.log_max_size);
        }
    }
    install_panic_hook();
    ms_log::log!(
        "Running mode: {}",
        if opts.daemon != 0 {
            "background"
        } else {
            "foreground"
        }
    );
    // print_version(1, 1): the banner through LOG0, the build lines through LOG.
    ms_log::log0!("{}", ms_config::VERSION_BANNER);
    for line in ms_config::built_info() {
        ms_log::log!("{line}");
    }
    ms_log::log!("Command line: {}", opts.command_line);
    for (index, module) in ms_log::LOG_LEVELS.iter().enumerate() {
        let mask = 1u32 << index;
        ms_log::logl!(mask, " LOG  of the module enabled: {module}");
        ms_log::debugl!(mask, "DEBUG of the module enabled: {module}");
    }
    if let Err(error) = runtime::run(&opts) {
        ms_log::log0!("{error}");
        sys::remove_pid_file();
        std::process::exit(1);
    }
}

/// The panic line names an unnamed thread `<unnamed>`, as std's own hook does;
/// normal log prefixes keep the C++ empty name.
fn panic_thread_name(name: String) -> String {
    if name.is_empty() {
        "<unnamed>".to_owned()
    } else {
        name
    }
}

/// Installs the panic hook: a `PANIC in thread ...` line through the logger
/// (the C++ `posix_signal_handler` prints `RECEIVED SIGNAL ...` instead).
fn install_panic_hook() {
    use std::io::Write as _;

    std::panic::set_hook(Box::new(|info| {
        let thread = panic_thread_name(ms_log::thread_name());
        let payload = info.payload();
        let message = if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_owned()
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            "<non-string panic payload>".to_owned()
        };
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "<unknown>".to_owned());
        let line = format!("PANIC in thread {thread}: {message} at {location}");
        // With a single -g the line only reaches syslog, and stderr may be the
        // same file as stdout (-g -f, or daemon -g where both are the -F
        // file), so always mirror it there. A second -g already mirrors
        // through LOG_PERROR/perror; do not print it twice.
        let slog = ms_log::slog();
        if slog == 1 || (slog == 0 && sys::stderr_differs_from_stdout()) {
            // `eprintln!` panics on a write error; a panic hook must not.
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }
        ms_log::write_panic(&line);
        dump_flight_recorder();
    }));
}

/// Part B dumps the flight recorder from the panic hook here.
fn dump_flight_recorder() {}

/// `get_command_line_string` joins all of argv with spaces, argv[0] included
/// (minisatip.cpp:591-606, 635).
fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `access("/proc/stb/frontend/0/fbc_connect", W_OK) == 0` (minisatip.cpp:1172).
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn fbc_host() -> bool {
    use ms_tuner::linux_dvb::{DeviceTree, LinuxDeviceTree};
    LinuxDeviceTree.writable("/proc/stb/frontend/0/fbc_connect")
}

#[cfg(not(all(feature = "linux-dvb", target_os = "linux")))]
fn fbc_host() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixDatagram;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::install_panic_hook;
    use super::sys;

    /// Set by a parent test to turn this process into a log child.
    const CHILD_MODE: &str = "MINISATIP_LOG_TEST_MODE";
    const CHILD_FILE: &str = "MINISATIP_LOG_TEST_FILE";
    const CHILD_SOCKET: &str = "MINISATIP_LOG_TEST_SOCKET";
    const CHILD_LIMIT: &str = "MINISATIP_LOG_TEST_LIMIT";
    const CHILD_TEST: &str = "tests::log_test_child";
    const PANIC_THREAD: &str = "AD3";
    const PANIC_MESSAGE: &str = "panic hook test";
    const LIMIT: u64 = 1000;

    #[test]
    fn panic_line_names_an_unnamed_thread() {
        assert_eq!(super::panic_thread_name(String::new()), "<unnamed>");
        assert_eq!(super::panic_thread_name("AD3".to_owned()), "AD3");
    }

    #[test]
    fn command_line_includes_the_program_name() {
        let args = ["-f".to_owned(), "-x".to_owned(), "8080".to_owned()];
        assert_eq!(
            super::command_line("/usr/bin/minisatip", &args),
            "/usr/bin/minisatip -f -x 8080"
        );
        assert_eq!(super::command_line("minisatip", &[]), "minisatip");
    }

    #[test]
    fn version_line_is_well_formed() {
        let line = format!("minisatip version {}", env!("CARGO_PKG_VERSION"));
        assert!(line.starts_with("minisatip version "));
    }

    /// Child half of the tests below: does nothing unless a parent asked for it.
    #[test]
    fn log_test_child() {
        let Ok(mode) = std::env::var(CHILD_MODE) else {
            return;
        };
        match mode.as_str() {
            "stdout" => {
                ms_log::set_thread_name("main");
                init_logger(sys::SysLogBackend::new(0), 0);
            }
            "file" => {
                let path = std::env::var(CHILD_FILE).expect("child log file");
                sys::redirect_to_log_file(&path).expect("redirect to the log file");
                ms_log::set_thread_name("main");
                init_logger(sys::SysLogBackend::new(0), 0);
            }
            "syslog" => {
                let path = std::env::var(CHILD_SOCKET).expect("child syslog socket");
                ms_log::set_thread_name("main");
                init_logger(sys::SysLogBackend::with_socket_path(1, path.as_str()), 1);
            }
            "rotate" => {
                run_rotation_child();
                return;
            }
            other => panic!("unknown {CHILD_MODE}: {other}"),
        }
        install_panic_hook();
        panic_on_named_thread();
    }

    fn init_logger(backend: sys::SysLogBackend, slog: u32) {
        ms_log::init(ms_log::Config {
            log: ms_log::LOG_GENERAL,
            debug: 0,
            slog,
            file_line: false,
            backend: Box::new(backend),
        });
    }

    fn panic_on_named_thread() {
        let handle = std::thread::Builder::new()
            .name(PANIC_THREAD.to_owned())
            .spawn(|| panic!("{PANIC_MESSAGE}"))
            .expect("spawn the panicking thread");
        assert!(handle.join().is_err());
    }

    fn run_rotation_child() {
        let path = std::env::var(CHILD_FILE).expect("child log file");
        let limit: u64 = std::env::var(CHILD_LIMIT)
            .expect("child log limit")
            .parse()
            .expect("numeric limit");
        sys::redirect_to_log_file(&path).expect("redirect to the log file");
        let backend = sys::SysLogBackend::new(0);
        let rotation = backend.rotation();
        rotation.start(&path, limit);
        ms_log::set_thread_name("main");
        init_logger(backend, 0);
        for index in 0..120 {
            ms_log::log0!("line {index}");
        }
    }

    fn child(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_MODE, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    #[test]
    fn panic_hook_writes_to_stdout() {
        let output = child("stdout").output().expect("run the child");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_panic_line(&String::from_utf8_lossy(&output.stdout));
    }

    #[test]
    fn panic_hook_writes_to_the_log_file() {
        let dir = unique_dir("panic-file");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("minisatip.log");
        let mut command = child("file");
        command.env(CHILD_FILE, &file);
        let output = command.output().expect("run the child");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = std::fs::read_to_string(&file).expect("log file");
        assert_panic_line(&text);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn panic_hook_writes_to_syslog() {
        let dir = unique_dir("panic-syslog");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("log");
        let socket = UnixDatagram::bind(&path).expect("bind the syslog socket");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut command = child("syslog");
        command.env(CHILD_SOCKET, &path);
        let output = command.output().expect("run the child");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut buffer = [0u8; 4096];
        let len = socket.recv(&mut buffer).expect("panic datagram");
        assert_panic_line(&String::from_utf8_lossy(&buffer[..len]));
        // With one -g the line also goes to stderr.
        assert_panic_line(&String::from_utf8_lossy(&output.stderr));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn panic_hook_mirrors_to_stderr_with_slog_even_when_merged() {
        let dir = unique_dir("panic-syslog-merged");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("log");
        let _socket = UnixDatagram::bind(&path).expect("bind the syslog socket");
        let merged = dir.join("merged.txt");
        let file = std::fs::File::create(&merged).expect("merged output");
        let mut command = child("syslog");
        command
            .env(CHILD_SOCKET, &path)
            // stdout and stderr are the same file: -g alone must still add
            // the stderr copy, since it otherwise only reaches syslog.
            .stdout(Stdio::from(file.try_clone().expect("clone merged output")))
            .stderr(Stdio::from(file));
        let output = command.output().expect("run the child");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = std::fs::read_to_string(&merged).expect("merged output");
        assert_panic_line(&text);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn log_max_size_rotates_at_the_limit() {
        let dir = unique_dir("rotate");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("minisatip.log");
        let mut command = child("rotate");
        command
            .env(CHILD_FILE, &file)
            .env(CHILD_LIMIT, LIMIT.to_string());
        let output = command.output().expect("run the child");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let backup = dir.join("minisatip.log.1");
        let current_text = std::fs::read_to_string(&file).expect("current log");
        let backup_text = std::fs::read_to_string(&backup).expect("rotated log");
        let longest =
            |text: &str| text.split_inclusive('\n').map(str::len).max().unwrap_or(0) as u64;
        let one_line = longest(&current_text).max(longest(&backup_text));
        for path in [&file, &backup] {
            let len = std::fs::metadata(path).expect("log metadata").len();
            assert!(len <= LIMIT + one_line, "{} is {len} bytes", path.display());
        }

        let first = |text: &str| text.lines().find_map(line_number).expect("line numbers");
        let last = |text: &str| {
            text.lines()
                .filter_map(line_number)
                .next_back()
                .expect("line numbers")
        };
        // A second rotation replaced the first chunk in `.1`.
        assert!(first(&backup_text) > 0, "{backup_text}");
        assert!(first(&current_text) > last(&backup_text), "{current_text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Asserts a panic line with the message, source location and thread name.
    fn assert_panic_line(text: &str) {
        let line = text
            .lines()
            .find(|line| line.contains(&format!("PANIC in thread {PANIC_THREAD}:")))
            .unwrap_or_else(|| panic!("no panic line in:\n{text}"));
        assert!(line.contains(PANIC_MESSAGE), "{line}");
        let mut fields = line
            .rsplit(" at ")
            .next()
            .unwrap_or_default()
            .rsplitn(3, ':');
        let column: u32 = fields
            .next()
            .and_then(|field| field.trim().parse().ok())
            .unwrap_or_else(|| panic!("no column: {line}"));
        let number: u32 = fields
            .next()
            .and_then(|field| field.parse().ok())
            .unwrap_or_else(|| panic!("no line number: {line}"));
        let file = fields.next().unwrap_or_default();
        assert!(file.ends_with("main.rs"), "{line}");
        assert!(number > 0 && column > 0, "{line}");
    }

    /// Sequence number of a `line N` log message.
    fn line_number(line: &str) -> Option<u64> {
        let start = line.rfind("line ")? + "line ".len();
        line[start..].trim().parse().ok()
    }

    fn unique_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "minisatip-log-test-{}-{nanos}-{name}",
            std::process::id()
        ))
    }
}
