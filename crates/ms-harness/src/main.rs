//! `ms-harness` command line.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ms_harness::{Binary, Comparison, RunOptions, Scenario, compare, run_once, tsgen};

const USAGE: &str = "\
usage:
  ms-harness run [options] [SCENARIO.yaml|DIR]...
      --rust BIN      Rust minisatip (default: next to ms-harness)
      --cpp BIN       C++ reference; compared as A against the Rust B
      --a BIN --b BIN compare two arbitrary binaries
      --runs N        repeat every scenario N times (default 1)
      --fast          only scenarios marked `fast: true`
      --filter TEXT   only scenarios whose name contains TEXT
      --golden DIR    also compare against DIR/<name>.txt
      --record DIR    write A's transcript to DIR/<name>.txt
      --html DIR      web root passed with -R (default: html)
      --work DIR      scratch directory (default: system temp)
      --captures DIR  recorded captures for `capture: file:<name>` scenarios
                      (skipped without it; harness/scenarios-real uses them)
      -v              print transcripts of passing scenarios
  ms-harness show --bin BIN SCENARIO.yaml   print one transcript
  ms-harness gen PROFILE OUT.ts             write a capture (fta, multi, pmtver, ca)
  ms-harness list [SCENARIO.yaml|DIR]...
Scenarios default to harness/scenarios.";

struct Args {
    command: String,
    rust: Option<PathBuf>,
    cpp: Option<PathBuf>,
    a: Option<PathBuf>,
    b: Option<PathBuf>,
    bin: Option<PathBuf>,
    runs: usize,
    fast: bool,
    verbose: bool,
    filter: Option<String>,
    golden: Option<PathBuf>,
    record: Option<PathBuf>,
    html: PathBuf,
    work: Option<PathBuf>,
    captures: Option<PathBuf>,
    paths: Vec<PathBuf>,
}

fn parse(mut raw: impl Iterator<Item = String>) -> Result<Args, String> {
    let command = raw.next().ok_or("missing command")?;
    let mut args = Args {
        command,
        rust: None,
        cpp: None,
        a: None,
        b: None,
        bin: None,
        runs: 1,
        fast: false,
        verbose: false,
        filter: None,
        golden: None,
        record: None,
        html: PathBuf::from("html"),
        work: None,
        captures: None,
        paths: Vec::new(),
    };
    while let Some(arg) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--rust" => args.rust = Some(value()?.into()),
            "--cpp" => args.cpp = Some(value()?.into()),
            "--a" => args.a = Some(value()?.into()),
            "--b" => args.b = Some(value()?.into()),
            "--bin" => args.bin = Some(value()?.into()),
            "--runs" => args.runs = value()?.parse().map_err(|_| "--runs needs a number")?,
            "--filter" => args.filter = Some(value()?),
            "--golden" => args.golden = Some(value()?.into()),
            "--record" => args.record = Some(value()?.into()),
            "--html" => args.html = value()?.into(),
            "--work" => args.work = Some(value()?.into()),
            "--captures" => args.captures = Some(value()?.into()),
            "--fast" => args.fast = true,
            "-v" | "--verbose" => args.verbose = true,
            "-h" | "--help" => return Err(String::new()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => args.paths.push(other.into()),
        }
    }
    Ok(args)
}

fn default_rust() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("minisatip")))
        .unwrap_or_else(|| PathBuf::from("target/debug/minisatip"))
}

fn binary(label: &str, path: &Path) -> Result<Binary, String> {
    let path =
        std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Binary {
        label: label.to_owned(),
        path,
    })
}

fn scenarios(args: &Args) -> Result<Vec<Scenario>, String> {
    let paths = if args.paths.is_empty() {
        vec![PathBuf::from("harness/scenarios")]
    } else {
        args.paths.clone()
    };
    Ok(Scenario::load(&paths)?
        .into_iter()
        .filter(|scenario| !args.fast || scenario.fast)
        .filter(|scenario| {
            args.filter
                .as_ref()
                .is_none_or(|text| scenario.name.contains(text.as_str()))
        })
        .collect())
}

fn options(args: &Args) -> Result<RunOptions, String> {
    let workroot = args
        .work
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("ms-harness-{}", std::process::id())));
    std::fs::create_dir_all(&workroot).map_err(|error| error.to_string())?;
    let workroot = std::fs::canonicalize(&workroot).map_err(|error| format!("--work: {error}"))?;
    let html = std::fs::canonicalize(&args.html)
        .map_err(|error| format!("--html {}: {error}", args.html.display()))?;
    let captures = match &args.captures {
        Some(dir) => Some(
            std::fs::canonicalize(dir)
                .map_err(|error| format!("--captures {}: {error}", dir.display()))?,
        ),
        None => None,
    };
    Ok(RunOptions {
        html,
        workroot,
        captures,
    })
}

fn run(args: &Args) -> Result<bool, String> {
    let (a, b) = match (&args.a, &args.b, &args.cpp) {
        (Some(a), Some(b), _) => (binary("a", a)?, binary("b", b)?),
        (None, None, Some(cpp)) => {
            let rust = args.rust.clone().unwrap_or_else(default_rust);
            (binary("cpp", cpp)?, binary("rust", &rust)?)
        }
        (None, None, None) => {
            let rust = args.rust.clone().unwrap_or_else(default_rust);
            (binary("rust", &rust)?, binary("rust", &rust)?)
        }
        _ => return Err("--a and --b go together and exclude --cpp".to_owned()),
    };
    let list = scenarios(args)?;
    if list.is_empty() {
        return Err("no scenarios selected".to_owned());
    }
    let options = options(args)?;
    let comparison = Comparison {
        a: &a,
        b: &b,
        runs: args.runs,
        golden: args.golden.as_deref(),
        record: args.record.as_deref(),
    };
    println!(
        "ms-harness: {} scenario(s), A={} B={}, {} run(s)",
        list.len(),
        a.path.display(),
        b.path.display(),
        args.runs.max(1)
    );
    let mut failed = Vec::new();
    let mut skipped = 0;
    for scenario in &list {
        if scenario.capture_file_name().is_some() && options.captures.is_none() {
            println!("SKIP {} (no --captures)", scenario.name);
            skipped += 1;
            continue;
        }
        let outcome = compare(scenario, &comparison, &options);
        println!(
            "{} {}",
            if outcome.passed { "PASS" } else { "FAIL" },
            outcome.name
        );
        if !outcome.passed || args.verbose {
            println!("{}", outcome.report);
        }
        if !outcome.passed {
            failed.push(outcome.name);
        }
    }
    println!(
        "ms-harness: {} passed, {} failed, {skipped} skipped{}",
        list.len() - failed.len() - skipped,
        failed.len(),
        if failed.is_empty() {
            String::new()
        } else {
            format!(" ({})", failed.join(", "))
        }
    );
    if args.work.is_none() && failed.is_empty() {
        let _ = std::fs::remove_dir_all(&options.workroot);
    }
    if skipped == list.len() {
        return Err("every selected scenario needs --captures".to_owned());
    }
    Ok(failed.is_empty())
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("ms-harness: {error}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command.as_str() {
        "run" => run(&args),
        "show" => (|| {
            let bin = args.bin.clone().unwrap_or_else(default_rust);
            let options = options(&args)?;
            for scenario in scenarios(&args)? {
                for line in run_once(&scenario, &binary("show", &bin)?, &options)? {
                    println!("{line}");
                }
            }
            Ok(true)
        })(),
        "gen" => match args.paths.as_slice() {
            [profile, out] => tsgen::generate(&profile.display().to_string())
                .ok_or_else(|| format!("unknown profile; known: {}", tsgen::PROFILES.join(", ")))
                .and_then(|data| std::fs::write(out, data).map_err(|error| error.to_string()))
                .map(|()| true),
            _ => Err("gen needs PROFILE OUT.ts".to_owned()),
        },
        "list" => scenarios(&args).map(|list| {
            for scenario in list {
                let fast = if scenario.fast { " [fast]" } else { "" };
                println!("{}{fast}: {}", scenario.name, scenario.description);
            }
            true
        }),
        other => Err(format!("unknown command {other}")),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("ms-harness: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_options() {
        let raw = [
            "run",
            "--cpp",
            "/x",
            "--runs",
            "10",
            "--fast",
            "--captures",
            "/c",
            "a.yaml",
        ];
        let args = parse(raw.iter().map(|s| s.to_string())).expect("parse");
        assert_eq!(args.captures.as_deref(), Some(Path::new("/c")));
        assert_eq!(args.runs, 10);
        assert!(args.fast);
        assert_eq!(args.cpp.as_deref(), Some(Path::new("/x")));
        assert_eq!(args.paths, vec![PathBuf::from("a.yaml")]);
        assert!(parse(["run", "--bogus"].iter().map(|s| s.to_string())).is_err());
    }
}
