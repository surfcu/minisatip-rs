//! Recording fake OSCam for DVBAPI scenarios (`oscam: true`): answers CLIENT_INFO,
//! filters each CA_PMT's ECM PIDs and logs one line per message (see docs/harness.md).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const CLIENT_INFO: u32 = 0xFFFF_0001;
const SERVER_INFO: u32 = 0xFFFF_0002;
const FILTER_DATA: u32 = 0xFFFF_0000;
const CA_PMT: u32 = 0x9F80_3282;
const CA_STOP: u32 = 0x9F80_3F04;
const DMX_SET_FILTER: u32 = 0x403C_6F2B;
const CMD_OK_DESCRAMBLING: u8 = 1;
const LIST_MANAGEMENT: [&str; 6] = ["MORE", "FIRST", "LAST", "ONLY", "ADD", "UPDATE"];

/// The recorded DVBAPI log, shared with the runner.
pub type Log = Arc<Mutex<Vec<String>>>;

/// A fake OSCam listening on 127.0.0.1.
pub struct FakeOscam {
    pub port: u16,
    pub log: Log,
}

impl FakeOscam {
    /// Listens on a free port and serves one client at a time; the thread lives
    /// until the harness exits.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let log: Log = Arc::default();
        let shared = Arc::clone(&log);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    return;
                };
                push(&shared, "connect".to_owned());
                serve(stream, &shared);
                push(&shared, "disconnect".to_owned());
            }
        });
        Ok(Self { port, log })
    }
}

fn push(log: &Log, line: String) {
    log.lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

/// Length of the message at the front of `buf`, or `None` when it is not complete.
fn message_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    let op = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = match op {
        CLIENT_INFO if buf.len() >= 7 => 7 + usize::from(buf[6]),
        CA_PMT if buf.len() >= 6 => 6 + usize::from(u16::from_be_bytes([buf[4], buf[5]])),
        FILTER_DATA if buf.len() >= 9 => {
            9 + ((usize::from(buf[7] & 0x0F) << 8) | usize::from(buf[8]))
        }
        CA_STOP => 8,
        CLIENT_INFO | CA_PMT | FILTER_DATA => return None,
        // Unknown framing: the rest cannot be split, so the caller ends the link.
        _ => buf.len(),
    };
    (buf.len() >= len).then_some(len)
}

fn serve(mut sock: TcpStream, log: &Log) {
    let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // (demux, filter) pairs whose data was recorded once already.
    let mut seen_data = BTreeSet::new();
    loop {
        let n = match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        while let Some(len) = message_len(&buf) {
            let msg: Vec<u8> = buf.drain(..len).collect();
            let op = u32::from_be_bytes([msg[0], msg[1], msg[2], msg[3]]);
            let mut reply = Vec::new();
            match op {
                CLIENT_INFO => {
                    let proto = u16::from_be_bytes([msg[4], msg[5]]);
                    push(log, format!("client_info proto={proto}"));
                    let info = b"OSCam harness";
                    reply.extend_from_slice(&SERVER_INFO.to_be_bytes());
                    reply.extend_from_slice(&2u16.to_be_bytes());
                    reply.push(info.len() as u8);
                    reply.extend_from_slice(info);
                }
                CA_PMT => {
                    let (line, filters) = ca_pmt(&msg);
                    push(log, line);
                    for (index, (key, demux, pid)) in filters.into_iter().enumerate() {
                        // A filter set again records its first data again.
                        seen_data.remove(&(demux, index as u8));
                        reply.extend_from_slice(&set_filter(key, demux, index as u8, pid));
                    }
                }
                FILTER_DATA => {
                    let (demux, filter) = (msg[4], msg[5]);
                    if seen_data.insert((demux, filter)) {
                        let table = msg.get(6).copied().unwrap_or(0);
                        push(
                            log,
                            format!(
                                "filter_data demux={demux} filter={filter} table={table:02x} len={}",
                                msg.len() - 6
                            ),
                        );
                    }
                }
                CA_STOP => push(log, format!("ca_stop demux={}", msg[7])),
                other => {
                    push(log, format!("op={other:08x} len={}", msg.len()));
                    return;
                }
            }
            if !reply.is_empty() && sock.write_all(&reply).is_err() {
                return;
            }
        }
    }
}

/// Decodes a CA_PMT: list management, program, command, descriptors and ES list.
/// Returns the line and the ECM filters to set (key, demux, ECM pid).
fn ca_pmt(msg: &[u8]) -> (String, Vec<(u8, u8, u16)>) {
    let lm = LIST_MANAGEMENT
        .get(usize::from(msg[6]))
        .copied()
        .unwrap_or("?");
    let sid = u16::from_be_bytes([msg[7], msg[8]]);
    let info_len = usize::from(u16::from_be_bytes([msg[10], msg[11]]) & 0x0FFF);
    let cmd = msg[12];
    // The adapter descriptor is the key id in network mode (dvbapi.cpp:497-499).
    let (mut key, mut demux) = (0u8, 0u8);
    let mut ecm_pids = Vec::new();
    let mut descriptors = Vec::new();
    let mut pos = 13;
    let end = (12 + info_len).min(msg.len());
    while pos + 2 <= end {
        let (tag, len) = (msg[pos], usize::from(msg[pos + 1]));
        let body = &msg[pos + 2..(pos + 2 + len).min(end)];
        match tag {
            0x83 if !body.is_empty() => key = body[0],
            0x86 if !body.is_empty() => demux = body[0],
            0x09 if body.len() >= 4 => {
                ecm_pids.push(u16::from_be_bytes([body[2], body[3]]) & 0x1FFF)
            }
            _ => {}
        }
        descriptors.push(format!("{tag:02x}:{}", hex(body)));
        pos += 2 + len;
    }
    let mut streams = Vec::new();
    let mut pos = end;
    while pos + 5 <= msg.len() {
        let pid = u16::from_be_bytes([msg[pos + 1], msg[pos + 2]]) & 0x1FFF;
        let es_len = usize::from(u16::from_be_bytes([msg[pos + 3], msg[pos + 4]]) & 0x0FFF);
        streams.push(format!("{:02x}:{pid:04x}", msg[pos]));
        pos += 5 + es_len;
    }
    let line = format!(
        "ca_pmt lm={lm} sid={sid:04x} cmd={cmd} desc=[{}] es=[{}]",
        descriptors.join(" "),
        streams.join(" ")
    );
    let filters = if cmd == CMD_OK_DESCRAMBLING {
        ecm_pids.into_iter().map(|pid| (key, demux, pid)).collect()
    } else {
        Vec::new()
    };
    (line, filters)
}

/// DMX_SET_FILTER for table 0x80 with mask 0xF0 (0x80 and 0x81 ECMs).
fn set_filter(key: u8, demux: u8, filter: u8, pid: u16) -> Vec<u8> {
    let mut f = vec![0u8; 65];
    f[..4].copy_from_slice(&DMX_SET_FILTER.to_be_bytes());
    f[4] = key;
    f[5] = demux;
    f[6] = filter;
    f[7..9].copy_from_slice(&pid.to_be_bytes());
    f[9] = 0x80;
    f[25] = 0xF0;
    f
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_ca_pmt() {
        // Built like dvbapi_send_pmt: 0x83 adapter, 0x84 pmt pid, 0x86 demux, 0x87 ca, 0x09 CA.
        let mut msg = CA_PMT.to_be_bytes().to_vec();
        msg.extend_from_slice(&[0, 0, 3, 0x00, 0x01, 0x01]);
        let info: Vec<u8> = vec![
            1, 0x83, 1, 0, 0x84, 2, 0x01, 0x00, 0x86, 1, 0, 0x87, 1, 0, 0x09, 4, 0x01, 0x00, 0xE1,
            0xF0,
        ];
        msg.extend_from_slice(&(info.len() as u16).to_be_bytes());
        msg.extend_from_slice(&info);
        msg.extend_from_slice(&[0x02, 0xE1, 0x01, 0, 0]);
        let len = (msg.len() - 6) as u16;
        msg[4..6].copy_from_slice(&len.to_be_bytes());
        assert_eq!(message_len(&msg), Some(msg.len()));
        let (line, filters) = ca_pmt(&msg);
        assert_eq!(
            line,
            "ca_pmt lm=ONLY sid=0001 cmd=1 desc=[83:00 84:0100 86:00 87:00 09:0100e1f0] es=[02:0101]"
        );
        assert_eq!(filters, [(0, 0, 0x1F0)]);
    }
}
