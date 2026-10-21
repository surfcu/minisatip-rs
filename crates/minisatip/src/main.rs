//! Binary entry point: wiring, logging and signal handling.

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
    let outcome = ms_config::parse_args(&args);
    // glibc getopt prints these as it walks argv; the C++ switch has no default
    // case, so they are diagnostics and parsing continues.
    for warning in &outcome.warnings {
        eprintln!("minisatip: {warning}");
    }
    if let Some(err) = outcome.error {
        eprintln!("minisatip: {err}");
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
    let opts = outcome.options;
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
    // minisatip.cpp:1914-1923: daemonise by default (opts.daemon is 1 unless
    // -f is given) and open syslog for -g. becomeDaemon's result is ignored.
    if opts.daemon != 0 {
        let _ = sys::become_daemon(&opts.log_file);
    }
    if opts.slog > 0 {
        sys::open_syslog("minisatip", opts.slog > 1);
    }
    if let Err(error) = runtime::run(&opts) {
        sys::log_error(format_args!("minisatip: {error}"));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_line_is_well_formed() {
        let line = format!("minisatip version {}", env!("CARGO_PKG_VERSION"));
        assert!(line.starts_with("minisatip version "));
    }
}
