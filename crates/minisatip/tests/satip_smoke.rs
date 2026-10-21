//! End-to-end smoke test for `-s`: a fake SAT>IP server feeds TS over RTP to
//! the binary, which streams the requested PIDs back to an RTSP client.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(15);
const TS_PACKET: usize = 188;
const SESSION: &str = "SATIPSESSION01";

struct FakeSatip {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeSatip {
    fn start() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || accept_loop(listener, &stop))
        };
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for FakeSatip {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept_loop(listener: TcpListener, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => serve(stream, stop),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

fn serve(mut stream: TcpStream, stop: &AtomicBool) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let client_port = Arc::new(AtomicU16::new(0));
    let sender_stop = Arc::new(AtomicBool::new(false));
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut sender: Option<thread::JoinHandle<()>> = None;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let read = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        };
        pending.extend_from_slice(&chunk[..read]);
        while let Some(end) = find(&pending, b"\r\n\r\n") {
            let request: Vec<u8> = pending.drain(..end + 4).collect();
            let text = String::from_utf8_lossy(&request).into_owned();
            let method = text.split_whitespace().next().unwrap_or("");
            let cseq = header(&text, "CSeq").unwrap_or_else(|| "1".to_owned());
            if method == "SETUP" {
                let port = header(&text, "Transport")
                    .and_then(|value| {
                        value
                            .split("client_port=")
                            .nth(1)?
                            .split('-')
                            .next()?
                            .parse()
                            .ok()
                    })
                    .unwrap_or(0);
                client_port.store(port, Ordering::SeqCst);
                let reply = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: {SESSION};timeout=60\r\n\
com.ses.streamID: 1\r\nTransport: RTP/AVP;unicast;destination=127.0.0.1;\
client_port={port}-{};server_port=5000-5001\r\n\r\n",
                    port.wrapping_add(1)
                );
                if stream.write_all(reply.as_bytes()).is_err() {
                    break;
                }
            } else if method == "PLAY" {
                let reply =
                    format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: {SESSION}\r\n\r\n");
                if stream.write_all(reply.as_bytes()).is_err() {
                    break;
                }
                if sender.is_none() {
                    sender = Some(spawn_stream(
                        Arc::clone(&client_port),
                        Arc::clone(&sender_stop),
                    ));
                }
            } else if method == "TEARDOWN" {
                sender_stop.store(true, Ordering::SeqCst);
                let reply =
                    format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: {SESSION}\r\n\r\n");
                if stream.write_all(reply.as_bytes()).is_err() {
                    break;
                }
            } else if method == "DESCRIBE" {
                let body = "tuner=1,215,1,12,0,0,0,0\r\n";
                let reply = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Type: application/sdp\r\n\
Content-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                if stream.write_all(reply.as_bytes()).is_err() {
                    break;
                }
            } else {
                let reply = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nPublic: OPTIONS, DESCRIBE, \
SETUP, PLAY, TEARDOWN\r\n\r\n"
                );
                if stream.write_all(reply.as_bytes()).is_err() {
                    break;
                }
            }
        }
    }
    sender_stop.store(true, Ordering::SeqCst);
    if let Some(sender) = sender {
        let _ = sender.join();
    }
}

fn spawn_stream(client_port: Arc<AtomicU16>, stop: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let socket = match UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let start = Instant::now();
        let mut seq = 0u16;
        while !stop.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(10) {
            let port = client_port.load(Ordering::SeqCst);
            if port != 0 {
                let _ = socket.send_to(&rtp_datagram(seq), (Ipv4Addr::LOCALHOST, port));
                seq = seq.wrapping_add(1);
            }
            thread::sleep(Duration::from_millis(2));
        }
    })
}

fn rtp_datagram(seq: u16) -> Vec<u8> {
    let mut datagram = vec![0x80, 0x21];
    datagram.extend_from_slice(&seq.to_be_bytes());
    datagram.extend_from_slice(&u32::from(seq).wrapping_mul(3600).to_be_bytes());
    datagram.extend_from_slice(&0x1122_3344u32.to_be_bytes());
    for index in 0..7 {
        let pid = if (index + usize::from(seq)) % 2 == 0 {
            0u16
        } else {
            100u16
        };
        let mut packet = [0xffu8; TS_PACKET];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        packet[2] = pid as u8;
        packet[3] = 0x10;
        datagram.extend_from_slice(&packet);
    }
    datagram
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

struct Server {
    child: Child,
    rtsp_port: u16,
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        terminate(&mut self.child);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Server {
    fn start(satip_port: u16) -> io::Result<Self> {
        let dir = unique_dir();
        fs::create_dir_all(&dir)?;
        let (rtsp_port, http_port) = free_tcp_ports()?;
        let start_rtp = free_rtp_block()?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_minisatip"))
            .args([
                "--disable-ssdp",
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
                "-s",
                &format!("127.0.0.1:{satip_port}"),
            ])
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
            rtsp_port,
            dir,
        })
    }
}

#[test]
fn satip_server_streams_to_rtsp_client() -> io::Result<()> {
    let fake = FakeSatip::start()?;
    let server = Server::start(fake.port)?;

    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    rtsp.set_write_timeout(Some(TIMEOUT))?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);
    let url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0,100");

    let (status, _, _) = request(
        &mut rtsp,
        &format!("DESCRIBE {url} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "DESCRIBE through the satip adapter");

    let client = UdpSocket::bind(("127.0.0.1", 0))?;
    client.set_read_timeout(Some(TIMEOUT))?;
    let client_port = client.local_addr()?.port();
    let (status, headers, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP;unicast;client_port={client_port}-{}\r\n\r\n",
            client_port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "SETUP");
    let session = header(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();

    let (status, headers, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "PLAY");
    assert!(headers.contains("RTP-Info:"), "PLAY headers: {headers}");

    let mut seen = [false; 2];
    let mut buffer = [0u8; 2048];
    for _ in 0..2000 {
        let (length, _) = client.recv_from(&mut buffer)?;
        if length < 12 + TS_PACKET {
            continue;
        }
        assert_eq!(buffer[0] >> 6, 2, "RTP version");
        assert_eq!(buffer[1] & 0x7f, 33, "payload type");
        let payload = &buffer[12..length];
        assert_eq!(
            payload.len() % TS_PACKET,
            0,
            "RTP payload must hold whole TS packets"
        );
        for packet in payload.chunks(TS_PACKET) {
            assert_eq!(packet[0], 0x47, "TS sync byte");
            let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
            match pid {
                0 => seen[0] = true,
                100 => seen[1] = true,
                other => panic!("received unrequested PID {other}"),
            }
        }
        if seen == [true, true] {
            break;
        }
    }
    assert_eq!(
        seen,
        [true, true],
        "PIDs must flow through the satip adapter"
    );

    let (status, _, _) = request(
        &mut rtsp,
        &format!("TEARDOWN {base} RTSP/1.0\r\nCSeq: 4\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "TEARDOWN");
    Ok(())
}

fn request(stream: &mut TcpStream, text: &str) -> io::Result<(u16, String, Vec<u8>)> {
    stream.write_all(text.as_bytes())?;
    stream.flush()?;
    read_response(stream)
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

// Start port plus the stream and satip RTP pairs must all be bindable.
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
    std::env::temp_dir().join(format!("minisatip-rs-satip-{}-{nanos}", std::process::id()))
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
