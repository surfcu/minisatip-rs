//! Startup logging test: the foreground binary must emit the C++ start-up
//! lines through the logger, with timestamps and the `main` thread name.
//!
//! The startup block runs before `runtime::run` (minisatip.cpp:1907-1932), so
//! the child needs no working adapter; stdout is captured to a file.

use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Server {
    child: Child,
    dir: PathBuf,
    output: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        terminate(&mut self.child);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Server {
    fn start() -> io::Result<Self> {
        let dir = unique_dir();
        fs::create_dir_all(&dir)?;
        let output = dir.join("stdout.log");
        let stdout = File::create(&output)?;
        let child = Command::new(env!("CARGO_BIN_EXE_minisatip"))
            .args([
                "-f",
                "--disable-ssdp",
                "-N",
                "-x",
                "0",
                "-y",
                "0",
                "-P",
                "0",
                "-z",
                dir.to_str().unwrap_or("."),
                "-l",
                "ca,pmt,ddci",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child, dir, output })
    }
}

#[test]
fn startup_lines_are_logged_with_timestamp_and_thread() -> io::Result<()> {
    let mut server = Server::start()?;

    let deadline = Instant::now() + TIMEOUT;
    let text = loop {
        let text = fs::read_to_string(&server.output).unwrap_or_default();
        if text.contains(" LOG  of the module enabled: ddci") {
            break text;
        }
        if let Some(status) = server.child.try_wait()? {
            panic!("minisatip exited during startup ({status}):\n{text}");
        }
        if Instant::now() >= deadline {
            panic!("startup lines did not appear:\n{text}");
        }
        thread::sleep(Duration::from_millis(25));
    };

    let messages: Vec<&str> = text
        .lines()
        .filter_map(|line| message(line, "main"))
        .collect();

    // minisatip.cpp:1921-1922: LOG("Command line: %s", ...).
    let command_line = messages
        .iter()
        .find(|message| message.starts_with("Command line: "))
        .unwrap_or_else(|| panic!("no Command line log in:\n{text}"));
    assert!(
        command_line.ends_with("-l ca,pmt,ddci"),
        "command line: {command_line}"
    );

    // minisatip.cpp:1925-1930: LOGL/DEBUGL for every enabled module; `-l`
    // adds to the default LOG_GENERAL mask (minisatip.cpp:1168-1169).
    for module in ["general", "pmt", "ca", "ddci"] {
        let expected = format!(" LOG  of the module enabled: {module}");
        assert!(
            messages.contains(&expected.as_str()),
            "missing {expected:?} in:\n{text}"
        );
    }
    assert!(
        !text.contains(" LOG  of the module enabled: http"),
        "http logging must stay disabled:\n{text}"
    );

    // The startup block is gone; stop the server now instead of waiting for
    // the harness to drop it.
    terminate(&mut server.child);
    Ok(())
}

/// Returns the message after `[MM/DD HH:MM:SS.mmm <thread>]: ` when the prefix
/// is well formed.
fn message<'line>(line: &'line str, thread: &str) -> Option<&'line str> {
    let rest = line.strip_prefix('[')?;
    let (stamp, rest) = rest.split_at_checked(18)?;
    let rest = rest.strip_prefix(' ')?;
    let (name, message) = rest.split_once("]: ")?;
    (name == thread && is_timestamp(stamp)).then_some(message)
}

/// `%02d/%02d %02d:%02d:%02d.%03d` (logging.cpp:115-116).
fn is_timestamp(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    bytes.len() == 18
        && bytes[0..2].iter().all(u8::is_ascii_digit)
        && bytes[2] == b'/'
        && bytes[3..5].iter().all(u8::is_ascii_digit)
        && bytes[5] == b' '
        && bytes[6..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b':'
        && bytes[9..11].iter().all(u8::is_ascii_digit)
        && bytes[11] == b':'
        && bytes[12..14].iter().all(u8::is_ascii_digit)
        && bytes[14] == b'.'
        && bytes[15..18].iter().all(u8::is_ascii_digit)
}

fn unique_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "minisatip-rs-logging-{}-{nanos}",
        std::process::id()
    ))
}

#[allow(unsafe_code)]
fn terminate(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    // SAFETY: SIGTERM to the child we spawned; the pid is valid while the
    // Child handle is alive and not yet reaped.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}
