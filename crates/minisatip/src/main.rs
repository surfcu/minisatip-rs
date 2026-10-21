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
    let mut opts = outcome.options;
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

/// `get_command_line_string` joins all of argv with spaces, argv[0] included
/// (minisatip.cpp:591-606, 635).
fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
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
