//! End-to-end CA test: a replayed CSA-scrambled service, a fake OSCam on the
//! DVBAPI socket and an RTSP client that must receive the clear packets.
#![cfg(feature = "dvbcsa")]

use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ms_ca_drivers::descramble::csa::Csa;
use ms_ca_drivers::descramble::{Cipher, KeyMaterial};
use ms_ts::crc32_mpeg2;

const TIMEOUT: Duration = Duration::from_secs(10);
const TS: usize = 188;
const PMT_PID: u16 = 0x100;
const VIDEO_PID: u16 = 0x101;
const ECM_PID: u16 = 0x500;
const SID: u16 = 0x64;
const CW: [u8; 8] = [0x11, 0x22, 0x33, 0x66, 0x44, 0x55, 0x66, 0xFF];
const VIDEO_PER_CYCLE: u8 = 20;

/// What the fake OSCam saw on the socket.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    ClientInfo,
    CaPmt { sid: u16, key: u8, demux: u8 },
    FilterData { demux: u8, filter: u8, table: u8 },
    Other(u32),
}

/// Answers like OSCam: SERVER_INFO, a filter on the ECM PID for each CAPMT,
/// and the control words once an ECM arrives.
fn fake_oscam(listener: TcpListener, seen: mpsc::Sender<Seen>) {
    let Ok((mut sock, _)) = listener.accept() else {
        return;
    };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut key = 0u8;
    let mut answered_ecm = false;
    loop {
        let Ok(n) = sock.read(&mut chunk) else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        while buf.len() >= 4 {
            let op = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
            let len = match op {
                0xFFFF_0001 if buf.len() >= 7 => 7 + usize::from(buf[6]),
                0x9F80_3282 if buf.len() >= 6 => {
                    6 + usize::from(u16::from_be_bytes([buf[4], buf[5]]))
                }
                0xFFFF_0000 if buf.len() >= 9 => {
                    9 + ((usize::from(buf[7] & 0x0F) << 8) | usize::from(buf[8]))
                }
                0x9F80_3F04 => 8,
                _ if buf.len() < 9 => break,
                _ => buf.len(),
            };
            if buf.len() < len {
                break;
            }
            let msg: Vec<u8> = buf.drain(..len).collect();
            let mut reply = Vec::new();
            match op {
                0xFFFF_0001 => {
                    let _ = seen.send(Seen::ClientInfo);
                    let info = b"OSCam v1.20, build r11720 (x86_64-linux)";
                    reply.extend_from_slice(&0xFFFF_0002u32.to_be_bytes());
                    reply.extend_from_slice(&2u16.to_be_bytes());
                    reply.push(info.len() as u8);
                    reply.extend_from_slice(info);
                }
                0x9F80_3282 => {
                    let sid = u16::from_be_bytes([msg[7], msg[8]]);
                    key = msg[15];
                    let demux = msg[22];
                    let _ = seen.send(Seen::CaPmt { sid, key, demux });
                    let mut f = vec![0u8; 65];
                    f[..4].copy_from_slice(&0x403C_6F2Bu32.to_be_bytes());
                    f[4] = key;
                    f[5] = demux;
                    f[6] = 0;
                    f[7..9].copy_from_slice(&ECM_PID.to_be_bytes());
                    f[9] = 0x80;
                    f[25] = 0xF0;
                    reply.extend_from_slice(&f);
                }
                0xFFFF_0000 => {
                    let _ = seen.send(Seen::FilterData {
                        demux: msg[4],
                        filter: msg[5],
                        table: msg[6],
                    });
                    if !answered_ecm {
                        answered_ecm = true;
                        for parity in 0..2u32 {
                            reply.extend_from_slice(&0x4010_6F86u32.to_be_bytes());
                            reply.push(key);
                            reply.extend_from_slice(&0u32.to_be_bytes());
                            reply.extend_from_slice(&parity.to_be_bytes());
                            reply.extend_from_slice(&CW);
                        }
                    }
                }
                other => {
                    let _ = seen.send(Seen::Other(other));
                }
            }
            if !reply.is_empty() && sock.write_all(&reply).is_err() {
                return;
            }
        }
    }
}

fn section_packets(pid: u16, section: &[u8], cc: &mut u8) -> Vec<u8> {
    let mut packet = [0xFFu8; TS];
    packet[0] = 0x47;
    packet[1] = 0x40 | (pid >> 8) as u8;
    packet[2] = pid as u8;
    packet[3] = 0x10 | (*cc & 0x0F);
    *cc = cc.wrapping_add(1);
    packet[4] = 0;
    packet[5..5 + section.len()].copy_from_slice(section);
    packet.to_vec()
}

fn with_crc(mut section: Vec<u8>) -> Vec<u8> {
    let len = section.len() + 4 - 3;
    section[1] = 0xB0 | (len >> 8) as u8;
    section[2] = len as u8;
    let crc = crc32_mpeg2(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

/// Plain payload of the `index`-th video packet of a cycle.
fn video_payload(index: u8) -> [u8; TS - 4] {
    std::array::from_fn(|j| index.wrapping_add((j as u8).wrapping_mul(3)))
}

fn is_plain_video(payload: &[u8]) -> bool {
    payload.len() == TS - 4 && payload[0] < VIDEO_PER_CYCLE && payload == video_payload(payload[0])
}

/// PAT, PMT with a CA descriptor, one ECM and CSA-scrambled (even key) video.
fn scrambled_capture(path: &PathBuf) -> io::Result<()> {
    let pat = with_crc(vec![
        0x00,
        0,
        0,
        0x00,
        0x01,
        0xC1,
        0,
        0, //
        (SID >> 8) as u8,
        SID as u8,
        0xE0 | (PMT_PID >> 8) as u8,
        PMT_PID as u8,
    ]);
    let pmt = with_crc(vec![
        0x02,
        0,
        0,
        (SID >> 8) as u8,
        SID as u8,
        0xC1,
        0,
        0, //
        0xE0 | (VIDEO_PID >> 8) as u8,
        VIDEO_PID as u8,
        0xF0,
        0x06, //
        0x09,
        0x04,
        0x0B,
        0x00,
        0xE0 | (ECM_PID >> 8) as u8,
        ECM_PID as u8, //
        0x02,
        0xE0 | (VIDEO_PID >> 8) as u8,
        VIDEO_PID as u8,
        0xF0,
        0x00,
    ]);
    let ecm = vec![0x80, 0x70, 0x08, 1, 2, 3, 4, 5, 6, 7, 8];
    let mut csa = Csa::new(false);
    let mut key = KeyMaterial {
        cw: [0; 32],
        iv: [0; 32],
        cw_len: 8,
        icam_ecm: 0,
    };
    key.cw[..8].copy_from_slice(&CW);
    csa.set_cw(&key);
    let (mut pat_cc, mut pmt_cc, mut ecm_cc, mut video_cc) = (0u8, 0u8, 0u8, 0u8);
    let mut data = Vec::new();
    for _ in 0..50 {
        data.extend(section_packets(0, &pat, &mut pat_cc));
        data.extend(section_packets(PMT_PID, &pmt, &mut pmt_cc));
        data.extend(section_packets(ECM_PID, &ecm, &mut ecm_cc));
        for index in 0..VIDEO_PER_CYCLE {
            let mut packet = [0u8; TS];
            packet[0] = 0x47;
            packet[1] = (VIDEO_PID >> 8) as u8;
            packet[2] = VIDEO_PID as u8;
            // Even key, payload only.
            packet[3] = 0x80 | 0x10 | (video_cc & 0x0F);
            video_cc = video_cc.wrapping_add(1);
            packet[4..].copy_from_slice(&video_payload(index));
            csa.encrypt(&mut [&mut packet[4..]]);
            data.extend_from_slice(&packet);
        }
    }
    fs::write(path, data)
}

struct Server {
    child: Child,
    dir: PathBuf,
    rtsp_port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn unique_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("minisatip-rs-ca-{}-{nanos}", std::process::id()))
}

fn free_port() -> io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn start_server(oscam_port: u16) -> io::Result<Server> {
    let dir = unique_dir();
    fs::create_dir_all(&dir)?;
    let capture = dir.join("scrambled.ts");
    scrambled_capture(&capture)?;
    let rtsp_port = free_port()?;
    let http_port = free_port()?;
    let log = fs::File::create(dir.join("stderr.log"))?;
    let child = Command::new(env!("CARGO_BIN_EXE_minisatip"))
        .args([
            "-V",
            "127.0.0.1",
            "--disable-ssdp",
            "-f",
            "-x",
            &http_port.to_string(),
            "-y",
            &rtsp_port.to_string(),
            "-z",
            dir.to_str().unwrap_or("."),
            "-e",
            "0",
            "-o",
            &format!("127.0.0.1:{oscam_port}"),
        ])
        .env("MINISATIP_REPLAY", &capture)
        .env("MINISATIP_REPLAY_BITRATE", "4000000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()?;
    let mut server = Server {
        child,
        dir,
        rtsp_port,
    };
    let deadline = Instant::now() + TIMEOUT;
    while TcpStream::connect(("127.0.0.1", rtsp_port)).is_err() {
        if Instant::now() > deadline || server.child.try_wait()?.is_some() {
            return Err(io::Error::other("minisatip did not start"));
        }
        thread::sleep(Duration::from_millis(25));
    }
    server.rtsp_port = rtsp_port;
    Ok(server)
}

fn rtsp(stream: &mut TcpStream, text: &str) -> io::Result<String> {
    stream.write_all(text.as_bytes())?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn wait_for(seen: &Receiver<Seen>, what: impl Fn(&Seen) -> bool, log: &mut Vec<Seen>) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match seen.recv_timeout(left) {
            Ok(event) => {
                let hit = what(&event);
                log.push(event);
                if hit {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

#[test]
fn scrambled_replay_is_descrambled_with_oscam_control_words() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let oscam_port = listener.local_addr()?.port();
    let (seen_tx, seen) = mpsc::channel();
    thread::spawn(move || fake_oscam(listener, seen_tx));
    let server = start_server(oscam_port)?;

    let mut control = TcpStream::connect(("127.0.0.1", server.rtsp_port))?;
    control.set_read_timeout(Some(TIMEOUT))?;
    let client = UdpSocket::bind(("127.0.0.1", 0))?;
    client.set_read_timeout(Some(Duration::from_millis(500)))?;
    let port = client.local_addr()?.port();
    let url = format!(
        "rtsp://127.0.0.1:{}/?src=1&freq=11720&pol=h&msys=dvbs2&sr=27500&pids=0,{PMT_PID},{VIDEO_PID}",
        server.rtsp_port
    );
    let head = rtsp(
        &mut control,
        &format!(
            "SETUP {url} RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port={port}-{}\r\n\r\n",
            port + 1
        ),
    )?;
    assert!(head.starts_with("RTSP/1.0 200"), "SETUP: {head}");
    let session = head
        .lines()
        .find_map(|l| l.strip_prefix("Session:"))
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_owned())
        .expect("Session header");
    let head = rtsp(
        &mut control,
        &format!("PLAY {url} RTSP/1.0\r\nCSeq: 2\r\nSession: {session}\r\n\r\n"),
    )?;
    assert!(head.starts_with("RTSP/1.0 200"), "PLAY: {head}");

    let mut log = Vec::new();
    assert!(
        wait_for(
            &seen,
            |s| matches!(s, Seen::CaPmt { sid: SID, .. }),
            &mut log
        ),
        "no CAPMT for the service: {log:?}"
    );
    assert!(
        wait_for(
            &seen,
            |s| matches!(s, Seen::FilterData { table: 0x80, .. }),
            &mut log
        ),
        "the ECM was not forwarded: {log:?}"
    );

    let mut clear = 0;
    let mut buffer = [0u8; 2048];
    let deadline = Instant::now() + TIMEOUT;
    while clear < 200 && Instant::now() < deadline {
        let Ok((len, _)) = client.recv_from(&mut buffer) else {
            continue;
        };
        for packet in buffer[12..len].chunks(TS) {
            let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
            if pid != VIDEO_PID {
                continue;
            }
            // Before OSCam registers, `ca_mask == 0` passes packets untouched;
            // afterwards the default `-E 1` drops undecrypted ones.
            if packet[3] & 0xC0 != 0 {
                assert_eq!(clear, 0, "scrambled packet after descrambling started");
                continue;
            }
            assert!(is_plain_video(&packet[4..]), "wrong clear payload");
            clear += 1;
        }
    }
    let stderr = fs::read_to_string(server.dir.join("stderr.log")).unwrap_or_default();
    assert!(
        clear >= 200,
        "only {clear} clear video packets; log: {log:?}\n{stderr}"
    );
    let _ = rtsp(
        &mut control,
        &format!("TEARDOWN {url} RTSP/1.0\r\nCSeq: 3\r\nSession: {session}\r\n\r\n"),
    );
    Ok(())
}
