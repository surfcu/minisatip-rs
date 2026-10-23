//! Binary entry point: wiring, logging and signal handling.

mod ca;
mod ddci;
mod dvbca;
mod hw;
mod runtime;
mod sys;

fn main() {
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
    ms_log::init(ms_log::Config {
        log: opts.log,
        debug: opts.debug,
        slog: opts.slog,
        file_line: opts.file_line != 0,
        backend: Box::new(sys::SysLogBackend::new(opts.slog)),
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
        let _ = sys::become_daemon(&opts.log_file);
    }
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
}
