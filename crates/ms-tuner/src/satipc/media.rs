//! Media reception: RTP datagrams (`satipc_read`) and RTSP-interleaved TCP
//! (`satipc_tcp_read`).

use super::client::Client;
use super::util::{find_bytes, find_ci, parse_int};
use crate::tuner::{TS_PACKET_SIZE, TsBuf};

/// `satipc.cpp`'s `#define DEFAULT_LOG LOG_SATIPC`.
const DEFAULT_LOG: u32 = ms_log::LOG_SATIPC;

/// `MAX_UDP_PACKET_SIZE`: the TS payload read from one datagram.
pub const MAX_UDP_PACKET_SIZE: usize = 7 * TS_PACKET_SIZE;
/// RTP header length assumed by satipc (no CSRC or extension).
pub const RTP_HEADER: usize = 12;
/// `MAX_RTP_MSG`: datagrams read per call.
pub const MAX_RTP_MSG: usize = 25;

/// Applies one RTP datagram: sequence statistics, then whole TS packets into `out`.
pub fn process_rtp_datagram(client: &mut Client, datagram: &[u8], out: &mut TsBuf) -> usize {
    if datagram.is_empty() {
        return 0;
    }
    client.rcvp = client.rcvp.wrapping_add(1);
    if datagram.len() >= 4 {
        let seq = u16::from_be_bytes([datagram[2], datagram[3]]);
        if client.rtp_seq == 0xFFFF {
            client.rtp_seq = seq;
        }
        if seq > client.rtp_seq {
            client.rtp_miss = client.rtp_miss.wrapping_add(1);
        } else if seq < client.rtp_seq {
            client.rtp_ooo = client.rtp_ooo.wrapping_add(1);
        }
        client.rtp_seq = seq.wrapping_add(1);
    }
    if datagram.len() <= RTP_HEADER {
        return 0;
    }
    let payload = &datagram[RTP_HEADER..datagram.len().min(RTP_HEADER + MAX_UDP_PACKET_SIZE)];
    // Some servers (e.g. XORO) send keep-alive RTP packets without TS payload.
    if payload.len() < MAX_UDP_PACKET_SIZE && payload[0] != 0x47 {
        // satipc.cpp:1103-1107; socket id/handle are not modelled here.
        ms_log::logm!(
            "discarding RTP packet without valid TS payload (sock {}, socket_id {}) [len={}]",
            0,
            0,
            payload.len()
        );
        return 0;
    }
    if client.ignore_packets {
        return 0;
    }
    push_packets(payload, out)
}

/// Appends the whole 188-byte packets of `payload` that fit in `out`.
pub fn push_packets(payload: &[u8], out: &mut TsBuf) -> usize {
    let mut appended = 0;
    for packet in payload.as_chunks::<TS_PACKET_SIZE>().0 {
        if !out.push_packet(packet) {
            break;
        }
        appended += TS_PACKET_SIZE;
    }
    appended
}

/// One unit found on an RTSP-interleaved connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Channel 0: the RTP payload after the 12-byte header.
    Rtp(Vec<u8>),
    /// Channel 1: an RTCP compound packet.
    Rtcp(Vec<u8>),
    /// An RTSP response, header and body.
    Rtsp(Vec<u8>),
}

/// Emits `satipc_tcp_read`'s skipped-bytes LOG for the amount not yet logged.
fn log_skipped(skipped: usize, logged: &mut usize) {
    if skipped > *logged {
        let delta = skipped - *logged;
        *logged = skipped;
        ms_log::log!("satipc_tcp_read: skipped {} bytes", delta);
    }
}

/// Splits the TCP byte stream like `satipc_tcp_read`, skipping unknown bytes.
#[derive(Debug, Default, Clone)]
pub struct TcpDemux {
    data: Vec<u8>,
    pos: usize,
    /// Bytes skipped while resynchronising.
    pub skipped: usize,
    /// How much of [`Self::skipped`] has already been logged.
    ///
    /// The C++ counter is a local of `satipc_tcp_read`; here frames are pulled
    /// one at a time, so the logged amount is tracked separately to keep the
    /// public total intact.
    logged_skipped: usize,
}

impl TcpDemux {
    /// Creates an empty demultiplexer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.pos > 0 {
            let pos = self.pos;
            self.data.drain(..pos);
            self.pos = 0;
            // satipc.cpp:1221
            ms_log::debugm!(
                "Moved from the position {}, length {}",
                pos,
                self.data.len()
            );
        }
        self.data.extend_from_slice(bytes);
    }

    /// Bytes not consumed yet.
    pub fn buffered(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Capacity of the buffered byte vector (`tcp_size`).
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Index of the next unconsumed byte (`tcp_pos`).
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Drops buffered bytes (`tcp_len = tcp_pos = 0`).
    pub fn reset(&mut self) {
        self.data.clear();
        self.pos = 0;
    }

    /// The next complete frame, or `None` until more data arrives.
    pub fn next_frame(&mut self) -> Option<Frame> {
        while self.pos + 6 < self.data.len() {
            let b = &self.data[self.pos..];
            if b[0] == 0x24 && b[1] < 2 && b[4] == 0x80 && (b[5] == 0x21 || b[5] == 0xC8) {
                let len = usize::from(u16::from_be_bytes([b[2], b[3]]));
                // satipc.cpp:1261-1262
                ms_log::debugm!(
                    "found at pos {}, rtsp_len {}, len {}",
                    self.pos,
                    len,
                    self.data.len()
                );
                // satipc.cpp:1270-1273
                log_skipped(self.skipped, &mut self.logged_skipped);
                if 4 + len > b.len() {
                    // satipc.cpp:1278-1284; destination position is not modelled.
                    ms_log::debugm!(
                        "The curent chunk is not completely read @ pos {}, tcp_pos {}, required {} len {} tcp_len {}, tcp_size {}, left to read {}",
                        0,
                        self.pos,
                        len.saturating_sub(RTP_HEADER),
                        b.len(),
                        self.data.len(),
                        self.data.capacity(),
                        4 + len - b.len()
                    );
                    return None;
                }
                let body = &b[4..4 + len];
                let frame = if b[1] == 1 {
                    Frame::Rtcp(body.to_vec())
                } else {
                    Frame::Rtp(body.get(RTP_HEADER..).unwrap_or(&[]).to_vec())
                };
                self.pos += 4 + len;
                return Some(frame);
            }
            if b.starts_with(b"RTSP") {
                // satipc.cpp:1308-1311
                log_skipped(self.skipped, &mut self.logged_skipped);
                let Some(nlnl) = find_bytes(b, b"\r\n\r\n") else {
                    // satipc.cpp:1334-1337
                    ms_log::logm!(
                        "End of rtsp rtsp message not found in this block, pos {}, tcp_pos {}, len {}, size {}",
                        0,
                        self.pos,
                        b.len(),
                        self.data.len()
                    );
                    return None;
                };
                let mut end = nlnl as i64;
                let mut icl = 0i32;
                if let Some(cl) = find_ci(&b[..nlnl], b"content-length:") {
                    let value = String::from_utf8_lossy(&b[cl + 15..nlnl]);
                    icl = parse_int(&value, 0);
                    end += i64::from(icl);
                }
                let total = usize::try_from(end + 4).unwrap_or(0).max(nlnl + 4);
                if total > b.len() {
                    return None;
                }
                let message = b[..total].to_vec();
                self.pos += total;
                // satipc.cpp:1354-1358; destination position is not modelled.
                ms_log::logm!(
                    "sending {} bytes to satipc_reply, cl {}, pos {}, tcp_len {}, left {}",
                    total,
                    icl,
                    self.pos - total,
                    self.data.len(),
                    0
                );
                return Some(Frame::Rtsp(message));
            }
            // satipc.cpp:1362-1364
            ms_log::debugm!("ignoring byte {:02X}", b[0]);
            self.skipped += 1;
            self.pos += 1;
        }
        if self.pos == self.data.len() {
            self.reset();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 0x21];
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&[0; 8]);
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn interleaved_frames_and_rtsp_are_split() {
        let mut demux = TcpDemux::new();
        let ts = [0x47u8; 188];
        let mut stream = vec![0x24, 0];
        let body = rtp(1, &ts);
        stream.extend_from_slice(&(body.len() as u16).to_be_bytes());
        stream.extend_from_slice(&body);
        stream.extend_from_slice(b"junk");
        stream.extend_from_slice(b"RTSP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nab");
        demux.push(&stream[..20]);
        assert_eq!(demux.next_frame(), None);
        demux.push(&stream[20..]);
        assert_eq!(demux.next_frame(), Some(Frame::Rtp(ts.to_vec())));
        assert_eq!(
            demux.next_frame(),
            Some(Frame::Rtsp(
                b"RTSP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nab".to_vec()
            ))
        );
        assert_eq!(demux.skipped, 4);
        assert_eq!(demux.buffered(), 0);
    }
}
