//! End-to-end smoke test: launched binary, replayed TS, RTSP + HTTP.
//!
//! Ports are allocated by the OS, the TS file is generated per run and every
//! socket operation has a timeout, so the test does not depend on the host.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(10);
const TS_PACKET: usize = 188;

struct Capture(PathBuf);

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Server {
    child: Child,
    rtsp_port: u16,
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
    fn start(replay: &Path) -> io::Result<Self> {
        let dir = unique_dir();
        fs::create_dir_all(&dir)?;
        let (rtsp_port, http_port) = free_tcp_ports()?;
        let start_rtp = free_udp_pair()?;
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
                "-e",
                "0",
            ])
            .env("MINISATIP_REPLAY", replay)
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
            http_port,
            dir,
        })
    }
}

fn start_server(capture: &Capture) -> io::Result<Server> {
    let mut last_error = None;
    for _ in 0..3 {
        match Server::start(&capture.0) {
            Ok(server) => return Ok(server),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("server did not start")))
}

#[test]
fn replay_serves_rtsp_and_http() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = start_server(&capture)?;

    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    rtsp.set_write_timeout(Some(TIMEOUT))?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);

    let (status, headers, _) = request(
        &mut rtsp,
        &format!("OPTIONS {base} RTSP/1.0\r\nCSeq: 1\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "OPTIONS");
    assert!(headers.contains("Public: OPTIONS"));

    let describe_url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0,100");
    let (status, headers, body) = request(
        &mut rtsp,
        &format!("DESCRIBE {describe_url} RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "DESCRIBE");
    assert!(headers.contains("application/sdp"));
    let sdp = String::from_utf8_lossy(&body);
    assert!(sdp.starts_with("v=0\r\n"), "SDP: {sdp}");
    assert!(sdp.contains("m=video"), "SDP: {sdp}");
    assert!(sdp.contains("a=fmtp:33"), "SDP: {sdp}");

    let client = UdpSocket::bind(("127.0.0.1", 0))?;
    client.set_read_timeout(Some(TIMEOUT))?;
    let client_port = client.local_addr()?.port();
    let (status, headers, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {describe_url} RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP;unicast;client_port={client_port}-{}\r\n\r\n",
            client_port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "SETUP");
    assert!(
        headers.contains(&format!("client_port={client_port}")),
        "SETUP headers: {headers}"
    );
    assert!(headers.contains("server_port="), "SETUP headers: {headers}");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();

    let (status, headers, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 4\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "PLAY");
    assert!(headers.contains("RTP-Info:"), "PLAY headers: {headers}");

    let mut seen = [false; 2];
    let mut buffer = [0u8; 2048];
    for _ in 0..500 {
        let (length, _) = client.recv_from(&mut buffer)?;
        if length < 12 {
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
    assert_eq!(seen, [true, true], "both requested PIDs must be streamed");

    let (status, _, _) = request(
        &mut rtsp,
        &format!("TEARDOWN {base} RTSP/1.0\r\nCSeq: 5\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "TEARDOWN");

    let (status, headers, body) = http_get(server.http_port, "/desc.xml")?;
    assert_eq!(status, 200, "GET /desc.xml");
    assert!(headers.contains("text/xml"), "desc headers: {headers}");
    let xml = String::from_utf8_lossy(&body);
    assert!(xml.contains("<friendlyName>"), "desc: {xml}");
    assert!(xml.contains("X_SATIPCAP"), "desc: {xml}");

    let (status, headers, body) = http_get(server.http_port, "/state.json")?;
    assert_eq!(status, 200, "GET /state.json");
    assert!(
        headers.contains("application/json"),
        "state headers: {headers}"
    );
    let json = String::from_utf8_lossy(&body);
    assert!(json.contains("\"ad_type\""), "state: {json}");
    assert!(json.contains("\"st_enabled\""), "state: {json}");

    Ok(())
}

#[test]
fn http_page_closes_connection() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let mut stream = TcpStream::connect(("127.0.0.1", server.http_port))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    let (status, _, _) = request(&mut stream, "GET /desc.xml HTTP/1.0\r\n\r\n")?;
    assert_eq!(status, 200);
    // read_http sets a 1 ms socket timeout for non-stream URLs (minisatip.cpp:1525).
    let mut rest = [0u8; 16];
    assert_eq!(
        stream.read(&mut rest)?,
        0,
        "server must close the connection"
    );
    Ok(())
}

#[test]
fn http_reget_hides_reply() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let mut stream = TcpStream::connect(("127.0.0.1", server.http_port))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    let (status, _, _) = request(
        &mut stream,
        "GET /?freq=11500&msys=dvbs2&sr=27500&pids=0 HTTP/1.0\r\n\r\n",
    )?;
    assert_eq!(status, 200);
    stream.write_all(b"GET /?addpids=100 HTTP/1.0\r\n\r\n")?;
    // http_response hides a 200 on a later read event (minisatip.cpp:2096-2102).
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + TIMEOUT;
    let has_pid_100 = |data: &[u8]| {
        data.windows(3)
            .any(|window| window[0] == 0x47 && window[1] & 0x1f == 0 && window[2] == 100)
    };
    while !has_pid_100(&data) && Instant::now() < deadline {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
    }
    assert!(has_pid_100(&data), "addpids must take effect");
    assert!(
        !data.windows(5).any(|window| window == b"HTTP/"),
        "no second HTTP header inside the stream"
    );
    Ok(())
}

fn request(stream: &mut TcpStream, text: &str) -> io::Result<(u16, String, Vec<u8>)> {
    stream.write_all(text.as_bytes())?;
    stream.flush()?;
    read_response(stream)
}

fn http_get(port: u16, path: &str) -> io::Result<(u16, String, Vec<u8>)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    request(
        &mut stream,
        &format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n"),
    )
}

fn read_response(stream: &mut TcpStream) -> io::Result<(u16, String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if buffer.len() > 1_000_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response too large",
            ));
        }
        let end = buffer.windows(4).position(|window| window == b"\r\n\r\n");
        if let Some(end) = end {
            let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
            let status = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status line"))?;
            let length = header_value(&head, "Content-Length")
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
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "response timeout"));
        }
    }
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

fn generate_ts() -> io::Result<Capture> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "minisatip-rs-capture-{}-{nanos}.ts",
        std::process::id()
    ));
    let mut file = fs::File::create(&path)?;
    let mut packets = Vec::new();
    for index in 0..400u16 {
        let pid = if index % 2 == 0 { 0u16 } else { 100u16 };
        let mut packet = [0xffu8; TS_PACKET];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8) & 0x1f;
        if index % 2 == 0 {
            packet[1] |= 0x40;
            packet[3] = 0x10;
        } else {
            packet[3] = 0x10;
        }
        packet[2] = pid as u8;
        packets.extend_from_slice(&packet);
    }
    file.write_all(&packets)?;
    Ok(Capture(path))
}

fn free_tcp_ports() -> io::Result<(u16, u16)> {
    for _ in 0..100 {
        let first = TcpListener::bind(("127.0.0.1", 0))?;
        let second = TcpListener::bind(("127.0.0.1", 0))?;
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

fn free_udp_pair() -> io::Result<u16> {
    for _ in 0..100 {
        let first = UdpSocket::bind(("127.0.0.1", 0))?;
        let port = first.local_addr()?.port();
        if port == 0 || port == u16::MAX {
            continue;
        }
        if let Ok(second) = UdpSocket::bind(("127.0.0.1", port + 1)) {
            drop(second);
            drop(first);
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free consecutive UDP ports",
    ))
}

fn unique_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("minisatip-rs-smoke-{}-{nanos}", std::process::id()))
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

#[test]
fn replay_fixture_has_expected_pids() -> io::Result<()> {
    let capture = generate_ts()?;
    let data = fs::read(&capture.0)?;
    assert_eq!(data.len(), 400 * TS_PACKET);
    let pids: Vec<u16> = data
        .chunks(TS_PACKET)
        .map(|packet| (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]))
        .collect();
    assert!(pids.contains(&0) && pids.contains(&100));
    Ok(())
}
