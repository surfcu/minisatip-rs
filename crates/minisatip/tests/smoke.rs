//! End-to-end smoke test: launched binary, replayed TS, RTSP + HTTP.
//!
//! Ports are allocated by the OS, the TS file is generated per run and every
//! socket operation has a timeout, so the test does not depend on the host.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(10);
const TS_PACKET: usize = 188;

/// Ports are freed just before the child binds them, so concurrent tests could
/// otherwise stop one server from starting after another took its ports.
static START_LOCK: Mutex<()> = Mutex::new(());

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
        Self::start_bound(replay, Some("127.0.0.1"))
    }

    fn start_bound(replay: &Path, bind: Option<&str>) -> io::Result<Self> {
        let _start_guard = START_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir = unique_dir();
        fs::create_dir_all(&dir)?;
        let (rtsp_port, http_port) = free_tcp_ports()?;
        let start_rtp = free_udp_pair()?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_minisatip"));
        if let Some(bind) = bind {
            command.args(["-V", bind]);
        }
        let mut child = command
            .args([
                "--disable-ssdp",
                // `-f`: stay in the foreground like C++ with FOREGROUND_OPT
                // (minisatip.cpp:704); the harness owns the child process.
                "-f",
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
    // A replay tuner stands in for a DVB-S/S2 frontend, so only those systems
    // are advertised (adapter.cpp:283-371, getAdaptersCount 478-535).
    assert!(xml.contains(">DVBS2-1</satip:X_SATIPCAP>"), "desc: {xml}");
    assert!(!xml.contains("DVBT-"), "desc: {xml}");
    assert!(!xml.contains("DVBC-"), "desc: {xml}");

    let (status, headers, body) = http_get(server.http_port, "/state.json")?;
    assert_eq!(status, 200, "GET /state.json");
    assert!(
        headers.contains("application/json"),
        "state headers: {headers}"
    );
    let json = String::from_utf8_lossy(&body);
    assert!(json.contains("\"ad_type\""), "state: {json}");
    assert!(json.contains("\"st_enabled\""), "state: {json}");
    // set_options: rtsp_host is always `lip:rtsp_port` (minisatip.cpp:1135-1136).
    let rtsp_host = format!("\"rtsp_host\":\"127.0.0.1:{}\"", server.rtsp_port);
    assert!(json.contains(&rtsp_host), "state: {json}");
    let http_host = format!("\"http_host\":\"127.0.0.1:{}\"", server.http_port);
    assert!(json.contains(&http_host), "state: {json}");

    Ok(())
}

#[test]
fn describe_uses_connection_address() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = Server::start_bound(&capture.0, None)?;
    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    let url = format!(
        "rtsp://127.0.0.1:{}/?freq=11500&msys=dvbs2&sr=27500&pids=0",
        server.rtsp_port
    );
    let (status, headers, body) = request(
        &mut rtsp,
        &format!("DESCRIBE {url} RTSP/1.0\r\nCSeq: 1\r\n\r\n"),
    )?;
    assert_eq!(status, 200);
    // describe_streams and Content-Base both use get_sock_shost (stream.cpp:97-105).
    assert!(
        headers.contains("Content-Base: rtsp://127.0.0.1/"),
        "{headers}"
    );
    let sdp = String::from_utf8_lossy(&body);
    assert!(sdp.contains(" IN IP4 127.0.0.1\r\n"), "SDP: {sdp}");
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

#[test]
fn setup_after_play_redirects_rtp() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    rtsp.set_write_timeout(Some(TIMEOUT))?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);
    let url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0");

    let first = UdpSocket::bind(("127.0.0.1", 0))?;
    first.set_read_timeout(Some(TIMEOUT))?;
    let first_port = first.local_addr()?.port();
    let (status, headers, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port={first_port}-{}\r\n\r\n",
            first_port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "SETUP");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();
    let (status, _, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "PLAY");
    let mut buffer = [0u8; 2048];
    first.recv_from(&mut buffer)?;

    let second = UdpSocket::bind(("127.0.0.1", 0))?;
    second.set_read_timeout(Some(TIMEOUT))?;
    let second_port = second.local_addr()?.port();
    let (status, _, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\nTransport: RTP/AVP;unicast;client_port={second_port}-{}\r\n\r\n",
            second_port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "second SETUP");
    // decode_transport creates a socket to the new client_port (stream.cpp:513-556);
    // process_dmx keeps flushing every enabled stream (stream.cpp:1189-1193).
    let (length, _) = second.recv_from(&mut buffer)?;
    assert!(length > 12, "RTP packet after redirect");
    Ok(())
}

#[test]
fn setup_after_play_switches_to_interleaved() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    rtsp.set_write_timeout(Some(TIMEOUT))?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);
    let url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0");

    let udp = UdpSocket::bind(("127.0.0.1", 0))?;
    udp.set_read_timeout(Some(TIMEOUT))?;
    let udp_port = udp.local_addr()?.port();
    let (status, headers, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port={udp_port}-{}\r\n\r\n",
            udp_port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "SETUP");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();
    let (status, _, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "PLAY");
    let mut packet = [0u8; 2048];
    udp.recv_from(&mut packet)?;

    // A later SETUP to RTP/AVP/TCP moves rsock_id to the RTSP socket
    // (stream.cpp:470-481); flush_stream then writes `$` frames on it.
    rtsp.write_all(
        format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n"
        )
        .as_bytes(),
    )?;
    rtsp.flush()?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let deadline = Instant::now() + TIMEOUT;
    while header_end.is_none() && Instant::now() < deadline {
        let read = rtsp.read(&mut chunk)?;
        assert!(read > 0, "connection closed");
        raw.extend_from_slice(&chunk[..read]);
        header_end = raw.windows(4).position(|window| window == b"\r\n\r\n");
    }
    let start = header_end.expect("SETUP response") + 4;
    let found = |data: &[u8]| {
        data[start..]
            .windows(5)
            .any(|window| window[0] == 0x24 && window[1] == 0 && window[4] >> 6 == 2)
    };
    while !found(&raw) && Instant::now() < deadline {
        let read = rtsp.read(&mut chunk)?;
        assert!(read > 0, "connection closed");
        raw.extend_from_slice(&chunk[..read]);
    }
    assert!(found(&raw), "no interleaved RTP after SETUP TCP");

    // The stream stays enabled and keeps sending (stream.cpp:470-481):
    // a stale stop event from the replaced UDP output must not close it.
    let first_count = count_frames(&raw[start..]);
    let deadline = Instant::now() + TIMEOUT;
    while count_frames(&raw[start..]) < first_count + 5 && Instant::now() < deadline {
        let read = rtsp.read(&mut chunk)?;
        assert!(read > 0, "connection closed after transport switch");
        raw.extend_from_slice(&chunk[..read]);
    }
    assert!(
        count_frames(&raw[start..]) >= first_count + 5,
        "stream stopped after switching to interleaved RTP"
    );
    Ok(())
}

#[test]
fn interleaved_reply_follows_a_complete_frame() -> io::Result<()> {
    // minisatip.cpp:2177 writes replies on the priority lane; flush_socket_prio
    // finishes the current `$` frame first (socketworks.cpp:1546-1561).
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    rtsp.set_write_timeout(Some(TIMEOUT))?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);
    let url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0");
    let (status, headers, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n"
        ),
    )?;
    assert_eq!(status, 200, "SETUP");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();
    let (status, _, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "PLAY");

    rtsp.write_all(
        format!("PLAY {base} RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\n\r\n").as_bytes(),
    )?;
    rtsp.flush()?;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let index = frame_end(&raw);
        if raw.len() > index && raw[index..].windows(4).any(|w| w == b"\r\n\r\n") {
            assert!(
                raw[index..].starts_with(b"RTSP/1.0 200"),
                "reply injected inside a $ frame"
            );
            break;
        }
        assert!(Instant::now() < deadline, "no reply after interleaved PLAY");
        let read = rtsp.read(&mut chunk)?;
        assert!(read > 0, "connection closed");
        raw.extend_from_slice(&chunk[..read]);
    }
    Ok(())
}

/// Returns the end of the leading run of complete `$` frames.
fn frame_end(data: &[u8]) -> usize {
    let mut index = 0;
    while index + 4 <= data.len() && data[index] == 0x24 && data[index + 1] < 2 {
        let length = usize::from(u16::from_be_bytes([data[index + 2], data[index + 3]]));
        if index + 4 + length > data.len() {
            break;
        }
        index += 4 + length;
    }
    index
}

/// Counts complete `$`-framed RTP/RTCP packets in `data`.
fn count_frames(data: &[u8]) -> usize {
    let mut index = 0;
    let mut count = 0;
    while index + 4 <= data.len() {
        if data[index] == 0x24 && data[index + 1] < 2 {
            let length = usize::from(u16::from_be_bytes([data[index + 2], data[index + 3]]));
            if index + 4 + length > data.len() {
                break;
            }
            count += 1;
            index += 4 + length;
        } else {
            index += 1;
        }
    }
    count
}

#[test]
fn repeated_play_reports_live_sequence() -> io::Result<()> {
    // minisatip.cpp:1416-1418 reports the stream's current `sid->seq`.
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let mut rtsp = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    rtsp.set_read_timeout(Some(TIMEOUT))?;
    rtsp.set_write_timeout(Some(TIMEOUT))?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);
    let url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0");
    let udp = UdpSocket::bind(("127.0.0.1", 0))?;
    udp.set_read_timeout(Some(TIMEOUT))?;
    let port = udp.local_addr()?.port();

    let (status, headers, _) = request(
        &mut rtsp,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port={port}-{}\r\n\r\n",
            port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "SETUP");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();
    let (status, first, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "first PLAY");
    let first_seq = rtp_info_seq(&first);
    let mut packet = [0u8; 2048];
    for _ in 0..20 {
        udp.recv_from(&mut packet)?;
    }

    let (status, second, _) = request(
        &mut rtsp,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "second PLAY");
    assert!(
        rtp_info_seq(&second) > first_seq,
        "RTP-Info did not advance: {} -> {}",
        rtp_info_seq(&second).min(first_seq),
        rtp_info_seq(&second)
    );
    Ok(())
}

fn rtp_info_seq(headers: &str) -> u32 {
    header_value(headers, "RTP-Info")
        .and_then(|line| line.split("seq=").nth(1).map(str::to_owned))
        .and_then(|value| value.split(';').next()?.parse().ok())
        .unwrap_or(0)
}

#[test]
fn duplicate_destination_stops_the_old_output() -> io::Result<()> {
    let capture = generate_ts()?;
    let server = start_server(&capture)?;
    let base = format!("rtsp://127.0.0.1:{}/", server.rtsp_port);
    let url = format!("{base}?freq=11500&msys=dvbs2&sr=27500&pids=0");
    let client = UdpSocket::bind(("127.0.0.1", 0))?;
    client.set_read_timeout(Some(Duration::from_millis(200)))?;
    let port = client.local_addr()?.port();

    let mut first = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    first.set_read_timeout(Some(TIMEOUT))?;
    first.set_write_timeout(Some(TIMEOUT))?;
    let (status, headers, _) = request(
        &mut first,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port={port}-{}\r\n\r\n",
            port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "first SETUP");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();
    let (status, _, _) = request(
        &mut first,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "first PLAY");
    let mut packet = [0u8; 2048];
    client.recv_from(&mut packet)?;

    // A second stream to the same destination closes the first one
    // (stream.cpp:591-605), including its RTP socket (stream.cpp:351-379).
    let mut second = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    second.set_read_timeout(Some(TIMEOUT))?;
    second.set_write_timeout(Some(TIMEOUT))?;
    let (status, headers, _) = request(
        &mut second,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port={port}-{}\r\n\r\n",
            port.wrapping_add(1)
        ),
    )?;
    assert_eq!(status, 200, "second SETUP");
    let session = header_value(&headers, "Session").expect("Session header");
    let session = session.split(';').next().unwrap_or("").trim().to_owned();
    let (status, _, _) = request(
        &mut second,
        &format!("PLAY {base} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert_eq!(status, 200, "second PLAY");

    // Drain anything the closed stream already sent, then sample the SSRCs.
    client.set_read_timeout(Some(Duration::from_millis(100)))?;
    let drain_until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain_until && client.recv_from(&mut packet).is_ok() {}
    let mut ssrcs = Vec::new();
    let sample_until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < sample_until {
        let Ok((length, _)) = client.recv_from(&mut packet) else {
            break;
        };
        if length >= 12 && !ssrcs.contains(&packet[8..12].to_vec()) {
            ssrcs.push(packet[8..12].to_vec());
        }
    }
    assert_eq!(ssrcs.len(), 1, "only the new stream may keep sending");
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

/// Finds the daemonised server by its unique `-y` port.
fn find_daemon_pid(rtsp_port: u16) -> Option<i32> {
    let port = rtsp_port.to_string();
    for entry in fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
        let binary = args.first().is_some_and(|arg| arg.ends_with(b"minisatip"));
        let port_matches = args
            .windows(2)
            .any(|pair| pair[0] == b"-y" && pair[1] == port.as_bytes());
        if binary && port_matches {
            return Some(pid);
        }
    }
    None
}

struct DaemonGuard(i32);

#[allow(unsafe_code)]
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if self.0 <= 0 {
            return;
        }
        // SAFETY: the pid was found by scanning /proc for our unique port and
        // SIGTERM only asks that process to shut down.
        unsafe {
            libc::kill(self.0, libc::SIGTERM);
        }
    }
}

#[allow(unsafe_code)]
#[test]
fn daemonises_by_default_and_redirects_stdio_to_the_logfile() -> io::Result<()> {
    // minisatip.cpp:1914-1916 daemonises unless FOREGROUND_OPT (-f) was
    // given; becomeDaemon double-forks, calls setsid and points stdin at
    // /dev/null with stdout/stderr on opts.log_file (utils.cpp:302-350).
    let _start_guard = START_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let capture = generate_ts()?;
    let dir = unique_dir();
    fs::create_dir_all(&dir)?;
    let log_path = dir.join("daemon.log");
    let (rtsp_port, http_port) = free_tcp_ports()?;
    let start_rtp = free_udp_pair()?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_minisatip"))
        .args([
            "--disable-ssdp",
            "-F",
            log_path.to_str().unwrap_or("."),
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
        .env("MINISATIP_REPLAY", &capture.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // The first fork's parent `_exit(0)` (utils.cpp:305-311), so the spawned
    // process returns immediately while the server keeps running.
    let status = child.wait()?;
    assert!(status.success(), "daemon parent status: {status}");

    let deadline = Instant::now() + TIMEOUT;
    let pid = loop {
        if let Some(pid) = find_daemon_pid(rtsp_port) {
            break pid;
        }
        if Instant::now() >= deadline {
            let _ = fs::remove_dir_all(&dir);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no daemon process appeared",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    let _guard = DaemonGuard(pid);

    // The log file is opened by becomeDaemon before the listener exists.
    let mut connected = false;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{rtsp_port}").parse().expect("addr"),
            Duration::from_millis(100),
        )
        .is_ok()
        {
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(connected, "daemonised server did not listen");
    assert!(log_path.exists(), "stdio must go to the -F log file");
    let _ = fs::remove_dir_all(&dir);
    Ok(())
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
