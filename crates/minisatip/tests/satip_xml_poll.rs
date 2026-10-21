//! `--satip-xml` polling (src_minisatip.cpp:1995-2007, src_satipc.cpp:2292-2303):
//! the XML is re-read and the adapters are added or removed when the tuner
//! counts change. `MINISATIP_SATIP_XML_POLL_MS` shortens the 120 s timer so the
//! test can observe two polls.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(15);
/// Poll well below the production 120 s.
const POLL_MS: u64 = 150;

fn capability(tuners: u32) -> String {
    format!(
        "HTTP/1.0 200 OK\r\nContent-Length: 0\r\nX-SATIP-RTSP-Port: 554\r\n\r\n\
<satip:X_SATIPCAP xmlns:satip=\"urn:ses-com:satip\">DVBS2-{tuners}</satip:X_SATIPCAP>"
    )
}

struct XmlServer {
    port: u16,
    body: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl XmlServer {
    fn start(tuners: u32) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let body = Arc::new(Mutex::new(capability(tuners)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let body = Arc::clone(&body);
            let stop = Arc::clone(&stop);
            thread::spawn(move || serve(listener, body, stop))
        };
        Ok(Self {
            port,
            body,
            stop,
            thread: Some(thread),
        })
    }

    fn set_tuners(&self, tuners: u32) {
        *self.body.lock().expect("xml body") = capability(tuners);
    }
}

impl Drop for XmlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(listener: TcpListener, body: Arc<Mutex<String>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                // Read the request head up to the blank line.
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let response = body.lock().expect("xml body").clone();
                let _ = stream.write_all(response.as_bytes());
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

struct Server {
    child: Child,
    http_port: u16,
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        terminate(&mut self.child);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Server {
    fn start(xml_port: u16) -> io::Result<Self> {
        let dir = unique_dir();
        fs::create_dir_all(&dir)?;
        let (rtsp_port, http_port) = free_tcp_ports()?;
        let start_rtp = free_rtp_block()?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_minisatip"))
            .args([
                "--disable-ssdp",
                // `-N`: ignore the host's /dev/dvb adapters so only the test's adapters count.
                "-N",
                // `-f`: stay in the foreground; the binary daemonises by
                // default (minisatip.cpp:1916, DEV-170), and the harness owns
                // the child process.
                "-f",
                "-V",
                "127.0.0.1",
                "-x",
                &http_port.to_string(),
                "-y",
                &rtsp_port.to_string(),
                "-P",
                &start_rtp.to_string(),
                "-z",
                dir.to_str().unwrap_or("."),
                "--satip-xml",
                &format!("http://127.0.0.1:{xml_port}/desc.xml"),
            ])
            .env("MINISATIP_SATIP_XML_POLL_MS", POLL_MS.to_string())
            .env_remove("MINISATIP_REPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "minisatip exited during startup: {status}"
                )));
            }
            if TcpStream::connect_timeout(
                &format!("127.0.0.1:{rtsp_port}").parse().expect("addr"),
                Duration::from_millis(100),
            )
            .is_ok()
            {
                break;
            }
            if Instant::now() >= deadline {
                terminate(&mut child);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "minisatip did not start listening",
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(Self {
            child,
            http_port,
            dir,
        })
    }

    fn satip_adapter_count(&self) -> io::Result<usize> {
        let (status, _, body) = http_get(self.http_port, "/state.json")?;
        assert_eq!(status, 200, "GET /state.json");
        let text = String::from_utf8_lossy(&body);
        let start = text
            .find("\"ad_type\":[")
            .map(|pos| pos + "\"ad_type\":[".len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no ad_type"))?;
        let end = text[start..]
            .find(']')
            .map(|pos| start + pos)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no ad_type end"))?;
        Ok(text[start..end]
            .split(',')
            .filter(|value| value.trim() == "1")
            .count())
    }
}

/// Polls until the runtime has `expected` adapters.
fn wait_for_adapters(server: &Server, expected: usize) -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let count = server.satip_adapter_count()?;
        if count == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("expected {expected} adapters, got {count}"),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn satip_xml_poll_adds_and_removes_adapters() -> io::Result<()> {
    let xml = XmlServer::start(1)?;
    let server = Server::start(xml.port)?;
    // The start-up `satip_getxml` pass adds the single advertised tuner.
    wait_for_adapters(&server, 1)?;

    // src_satipc.cpp:2265-2282: more tuners than configured -> disable and add.
    xml.set_tuners(2);
    wait_for_adapters(&server, 2)?;

    // Fewer tuners -> the first poll removes the extra adapter.
    xml.set_tuners(1);
    wait_for_adapters(&server, 1)?;
    Ok(())
}

fn http_get(port: u16, path: &str) -> io::Result<(u16, String, Vec<u8>)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())?;
    read_response(&mut stream)
}

fn read_response(stream: &mut TcpStream) -> io::Result<(u16, String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(end) = find(&buffer, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
            let status = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status line"))?;
            let length = header(&head, "Content-Length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if buffer.len() >= end + 4 + length {
                return Ok((status, head, buffer[end + 4..end + 4 + length].to_vec()));
            }
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the response completed",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 1_000_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response too large",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "response timeout"));
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn header(text: &str, name: &str) -> Option<String> {
    text.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

fn free_tcp_ports() -> io::Result<(u16, u16)> {
    for _ in 0..100 {
        let first = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let second = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let first_port = first.local_addr()?.port();
        let second_port = second.local_addr()?.port();
        if first_port != second_port {
            return Ok((first_port, second_port));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no distinct TCP ports",
    ))
}

fn free_rtp_block() -> io::Result<u16> {
    for _ in 0..100 {
        let start = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        let port = start.local_addr()?.port();
        if port > u16::MAX - 1001 {
            continue;
        }
        let other = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port + 1));
        let satip = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port + 1000));
        let satip_rtcp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port + 1001));
        if other.is_ok() && satip.is_ok() && satip_rtcp.is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free RTP block",
    ))
}

fn unique_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "minisatip-rs-satip-xml-{}-{nanos}",
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
