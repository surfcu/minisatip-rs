//! Launching a server binary (C++ or Rust) on free local ports.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

const START_TIMEOUT: Duration = Duration::from_secs(10);
/// UDP ports reserved above `-P` for server RTP/RTCP sockets.
pub const RTP_RANGE: u16 = 32;

/// A binary under test.
#[derive(Debug, Clone)]
pub struct Binary {
    /// Label used in reports ("cpp", "rust", "A", ...).
    pub label: String,
    pub path: PathBuf,
}

/// Inputs shared by both binaries for one scenario run.
pub struct Launch<'a> {
    pub capture: &'a Path,
    pub adapters: usize,
    pub bitrate: u64,
    pub args: &'a [String],
    pub html: &'a Path,
    pub workdir: &'a Path,
    /// Fake CAM socket for `MINISATIP_REPLAY_CA`.
    pub ca_socket: Option<&'a Path>,
}

/// A running server; stopped on drop.
pub struct Server {
    child: Child,
    pub rtsp_port: u16,
    pub http_port: u16,
    pub start_rtp: u16,
    pub log: PathBuf,
    _rtp_lease: PortLease,
}

impl Drop for Server {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

impl Server {
    /// Starts `binary` and waits until both listeners accept connections.
    pub fn start(binary: &Binary, launch: &Launch<'_>) -> io::Result<Self> {
        let mut last = None;
        for _ in 0..3 {
            match Self::try_start(binary, launch) {
                Ok(server) => return Ok(server),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("server did not start")))
    }

    fn try_start(binary: &Binary, launch: &Launch<'_>) -> io::Result<Self> {
        let cache = launch.workdir.join(format!("{}-cache", binary.label));
        let _ = fs::remove_dir_all(&cache);
        fs::create_dir_all(&cache)?;
        let (rtsp_port, http_port) = free_tcp_pair()?;
        // TCP and UDP numbers can coincide; keep them apart so placeholders are unique.
        let rtp_lease = free_udp_range(RTP_RANGE, &[rtsp_port, http_port])?;
        let start_rtp = rtp_lease.base();
        let log = launch.workdir.join(format!("{}.log", binary.label));
        let log_file = fs::File::create(&log)?;
        let mut args: Vec<String> = vec![
            "-f".into(),
            "-G".into(),
            "-V".into(),
            "127.0.0.1".into(),
            "-x".into(),
            http_port.to_string(),
            "-y".into(),
            rtsp_port.to_string(),
            "-P".into(),
            start_rtp.to_string(),
            "-z".into(),
            cache.display().to_string(),
            "-R".into(),
            launch.html.display().to_string(),
            "-e".into(),
            if launch.adapters == 1 {
                "0".to_owned()
            } else {
                format!("0-{}", launch.adapters - 1)
            },
        ];
        args.extend(launch.args.iter().cloned());
        let mut command = Command::new(&binary.path);
        match launch.ca_socket {
            Some(path) => command.env("MINISATIP_REPLAY_CA", path),
            None => command.env_remove("MINISATIP_REPLAY_CA"),
        };
        let mut child = command
            .args(&args)
            .current_dir(launch.workdir)
            .env("MINISATIP_REPLAY", launch.capture)
            .env("MINISATIP_REPLAY_BITRATE", launch.bitrate.to_string())
            .env("MINISATIP_REPLAY_ADAPTERS", launch.adapters.to_string())
            .stdin(Stdio::null())
            .stdout(log_file.try_clone()?)
            .stderr(log_file)
            .spawn()
            .map_err(|error| io::Error::other(format!("{}: {error}", binary.path.display())))?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "{} exited during startup ({status}), see {}",
                    binary.label,
                    log.display()
                )));
            }
            if listening(rtsp_port) && listening(http_port) {
                break;
            }
            if Instant::now() >= deadline {
                stop(&mut child);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} did not start listening", binary.label),
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(Self {
            child,
            rtsp_port,
            http_port,
            start_rtp,
            _rtp_lease: rtp_lease,
            log,
        })
    }
}

fn listening(port: u16) -> bool {
    let address = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok()
}

/// SIGTERM through `kill(1)` (no `unsafe`), then SIGKILL after a grace period.
fn stop(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn free_tcp_pair() -> io::Result<(u16, u16)> {
    let first = TcpListener::bind(("127.0.0.1", 0))?;
    let second = TcpListener::bind(("127.0.0.1", 0))?;
    Ok((first.local_addr()?.port(), second.local_addr()?.port()))
}

/// UDP ports leased to a server RTP range or a client pair in this process.
static LEASED: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

/// Exclusive claim on UDP `base..base+count` among parallel runs; released on drop.
#[derive(Debug)]
pub struct PortLease {
    base: u16,
    count: u16,
}

impl PortLease {
    fn try_new(base: u16, count: u16) -> Option<Self> {
        let mut leased = LEASED.lock().unwrap_or_else(PoisonError::into_inner);
        let ports = base..base.checked_add(count)?;
        if leased.range(ports.clone()).next().is_some() {
            return None;
        }
        leased.extend(ports);
        Some(Self { base, count })
    }

    pub fn base(&self) -> u16 {
        self.base
    }
}

impl Drop for PortLease {
    fn drop(&mut self) {
        let mut leased = LEASED.lock().unwrap_or_else(PoisonError::into_inner);
        for port in self.base..self.base + self.count {
            leased.remove(&port);
        }
    }
}

/// Leases UDP `p..p+count`, all free right now, containing none of the `avoid`
/// ports and no port leased by another run (A and B run in parallel).
pub fn free_udp_range(count: u16, avoid: &[u16]) -> io::Result<PortLease> {
    for _ in 0..200 {
        let probe = UdpSocket::bind(("127.0.0.1", 0))?;
        let base = probe.local_addr()?.port() & !1;
        drop(probe);
        if base < 1024 || base.checked_add(count).is_none() || overlaps(base, count, avoid) {
            continue;
        }
        let Some(lease) = PortLease::try_new(base, count) else {
            continue;
        };
        let held: Vec<_> = (base..base + count)
            .map_while(|port| UdpSocket::bind(("127.0.0.1", port)).ok())
            .collect();
        if held.len() == usize::from(count) {
            return Ok(lease);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free UDP port range",
    ))
}

fn overlaps(base: u16, count: u16, avoid: &[u16]) -> bool {
    avoid.iter().any(|port| port.wrapping_sub(base) < count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_covers_exactly_the_range() {
        assert!(overlaps(40000, 32, &[40000]));
        assert!(overlaps(40000, 32, &[1, 40031]));
        assert!(!overlaps(40000, 32, &[39999, 40032]));
        assert!(!overlaps(40000, 32, &[]));
        assert!(overlaps(u16::MAX - 1, 32, &[u16::MAX]));
    }

    #[test]
    fn leases_are_exclusive_until_dropped() {
        // Below the ephemeral range, so leases from parallel tests never meet these.
        let first = PortLease::try_new(1100, 4).expect("free");
        assert!(PortLease::try_new(1102, 4).is_none());
        let second = PortLease::try_new(1104, 2).expect("adjacent range");
        drop(first);
        let third = PortLease::try_new(1102, 2).expect("released");
        assert!(PortLease::try_new(1105, 1).is_none());
        assert!(PortLease::try_new(u16::MAX, 2).is_none());
        drop((second, third));
    }

    #[test]
    fn leased_ports_are_not_handed_out_again() {
        let lease = free_udp_range(2, &[]).unwrap();
        for _ in 0..50 {
            let other = free_udp_range(2, &[]).unwrap();
            assert!(!overlaps(
                other.base(),
                2,
                &[lease.base(), lease.base() + 1]
            ));
        }
    }

    #[test]
    fn free_udp_range_never_returns_an_avoided_port() {
        let all: Vec<u16> = (0..=u16::MAX).collect();
        assert!(free_udp_range(2, &all).is_err());
    }

    #[test]
    fn free_udp_range_skips_avoided_ports() {
        let (rtsp, http) = free_tcp_pair().unwrap();
        for _ in 0..20 {
            let base = free_udp_range(RTP_RANGE, &[rtsp, http]).unwrap().base();
            assert!(
                !overlaps(base, RTP_RANGE, &[rtsp, http]),
                "{base} {rtsp} {http}"
            );
        }
    }
}
