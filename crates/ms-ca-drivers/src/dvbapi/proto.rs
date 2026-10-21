//! DVBAPI wire format: opcodes, client message builders, server message parser.

use ms_ca::{CaDescriptor, StreamPid};

/// Protocol version this client speaks (`DVBAPI_PROTOCOL_VERSION`).
pub const DVBAPI_PROTOCOL_VERSION: u16 = 2;

/// Section data sent to the server.
pub const DVBAPI_FILTER_DATA: u32 = 0xFFFF_0000;
/// Client handshake.
pub const DVBAPI_CLIENT_INFO: u32 = 0xFFFF_0001;
/// Server handshake reply.
pub const DVBAPI_SERVER_INFO: u32 = 0xFFFF_0002;
/// ECM statistics for a key.
pub const DVBAPI_ECM_INFO: u32 = 0xFFFF_0003;
/// Start a section filter.
pub const DVBAPI_DMX_SET_FILTER: u32 = 0x403C_6F2B;
/// Assign a PID to a descrambler index.
pub const DVBAPI_CA_SET_PID: u32 = 0x4008_6F87;
/// CSA control word.
pub const DVBAPI_CA_SET_DESCR: u32 = 0x4010_6F86;
/// AES control word (16 bytes).
pub const DVBAPI_CA_SET_DESCR_AES: u32 = 0x4010_6F87;
/// Descrambling algorithm and cipher mode.
pub const DVBAPI_CA_SET_DESCR_MODE: u32 = 0x400C_6F88;
/// Stop a section filter.
pub const DVBAPI_DMX_STOP: u32 = 0x0000_6F2A;

// Low 24 bits of opcodes sent in host (little-endian) order after the key byte.
const CA_SET_DESCR_X: u32 = 0x86_6F10;
const CA_SET_DESCR_AES_X: u32 = 0x87_6F10;
const CA_SET_PID_X: u32 = 0x87_6F08;
const DMX_STOP_X: u32 = 0x2A_6F00;
const DMX_SET_FILTER_X: u32 = 0x2B_6F3C;

/// CA PMT application object tag plus the 2-byte length marker.
pub const AOT_CA_PMT: u32 = 0x9F80_3282;

/// `ca_pmt_list_management` values (C++ `CLM_*`).
pub const CLM_MORE: u8 = 0x00;
/// First object of a list.
pub const CLM_FIRST: u8 = 0x01;
/// Last object of a list.
pub const CLM_LAST: u8 = 0x02;
/// Single object replacing the list.
pub const CLM_ONLY: u8 = 0x03;
/// Object appended to the list.
pub const CLM_ADD: u8 = 0x04;
/// Object replacing an entry of the list.
pub const CLM_UPDATE: u8 = 0x05;

/// `ca_pmt_cmd_id` ok_descrambling.
pub const CMD_ID_OK_DESCRAMBLING: u8 = 0x01;
/// `ca_pmt_cmd_id` not_selected.
pub const CMD_ID_NOT_SELECTED: u8 = 0x04;

/// `CA_SET_DESCR_MODE` algorithms.
pub const CW_ALGO_CSA: i32 = 0;
/// DES.
pub const CW_ALGO_DES: i32 = 1;
/// AES-128.
pub const CW_ALGO_AES128: i32 = 2;
/// CSA (alternative id).
pub const CW_ALGO_CSA_ALT: i32 = 3;
/// ECB cipher mode.
pub const CW_ALGO_MODE_ECB: i32 = 0;
/// CBC cipher mode.
pub const CW_ALGO_MODE_CBC: i32 = 1;

/// Size of the C++ CAPMT buffer; descriptors and streams are cut to fit.
pub const MAX_CAPMT_LEN: usize = 2500;
/// Longest section forwarded as FILTER_DATA.
pub const MAX_FILTER_SECTION: usize = 559 + 3;
/// ECM_INFO strings are cut to this length (`sizeof(cardsystem) - 2`).
pub const ECM_INFO_STR_MAX: usize = 62;

/// `DVBAPI_CLIENT_INFO`: opcode, version, length-prefixed client name.
pub fn client_info(version: u16, name: &str) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(255)];
    let mut b = Vec::with_capacity(7 + name.len());
    b.extend_from_slice(&DVBAPI_CLIENT_INFO.to_be_bytes());
    b.extend_from_slice(&version.to_be_bytes());
    b.push(name.len() as u8);
    b.extend_from_slice(name);
    b
}

/// CA_STOP for one demux, or for all of them with `demux = 0xFF`.
pub fn ca_stop(demux: u8) -> [u8; 8] {
    [0x9F, 0x80, 0x3F, 0x04, 0x83, 0x02, 0x00, demux]
}

/// `DVBAPI_FILTER_DATA` carrying one section; `None` if it is too long.
pub fn filter_data(demux: u8, filter: u8, section: &[u8]) -> Option<Vec<u8>> {
    if section.len() > MAX_FILTER_SECTION {
        return None;
    }
    let mut b = Vec::with_capacity(6 + section.len());
    b.extend_from_slice(&DVBAPI_FILTER_DATA.to_be_bytes());
    b.push(demux);
    b.push(filter);
    b.extend_from_slice(section);
    Some(b)
}

/// Fields of one CAPMT object as `dvbapi_send_pmt` fills them.
#[derive(Clone, Copy, Debug)]
pub struct Capmt<'a> {
    /// `ca_pmt_list_management`.
    pub list_management: u8,
    /// `ca_pmt_cmd_id`.
    pub cmd_id: u8,
    /// Program number.
    pub sid: u16,
    /// Adapter device descriptor (0x83) value.
    pub adapter: u8,
    /// Demux (0x86) and CA device (0x87) descriptor value.
    pub demux: u8,
    /// PMT PID descriptor (0x84) value.
    pub pmt_pid: u16,
    /// CA descriptors.
    pub ca: &'a [CaDescriptor],
    /// Elementary streams.
    pub streams: &'a [StreamPid],
}

/// Builds a CAPMT byte for byte like `dvbapi_send_pmt`.
pub fn build_capmt(c: &Capmt<'_>) -> Vec<u8> {
    let mut b = vec![0u8; 26];
    b[0..4].copy_from_slice(&AOT_CA_PMT.to_be_bytes());
    b[6] = c.list_management;
    b[7..9].copy_from_slice(&c.sid.to_be_bytes());
    b[9] = 1;
    b[12] = c.cmd_id;
    b[13..16].copy_from_slice(&[0x83, 0x01, c.adapter]);
    b[16..18].copy_from_slice(&[0x84, 0x02]);
    b[18..20].copy_from_slice(&c.pmt_pid.to_be_bytes());
    b[20..23].copy_from_slice(&[0x86, 0x01, c.demux]);
    b[23..26].copy_from_slice(&[0x87, 0x01, c.demux]);

    let mut ca_len = 0;
    for d in c.ca {
        let private = d.private_data.len();
        if ca_len + 6 + private > MAX_CAPMT_LEN - 26 {
            break;
        }
        b.push(0x09);
        b.push((0x04 + private) as u8);
        b.extend_from_slice(&d.caid.to_be_bytes());
        b.extend_from_slice(&d.pid.to_be_bytes());
        b.extend_from_slice(&d.private_data);
        ca_len += 6 + private;
    }
    let program_info_length = (b.len() - 12) as u16;
    b[10..12].copy_from_slice(&program_info_length.to_be_bytes());

    for s in c.streams {
        if b.len() + 5 > MAX_CAPMT_LEN {
            break;
        }
        b.push(s.stream_type);
        b.extend_from_slice(&s.pid.to_be_bytes());
        b.extend_from_slice(&[0, 0]);
    }
    let length = (b.len() - 6) as u16;
    b[4..6].copy_from_slice(&length.to_be_bytes());
    b
}

/// ECM statistics reported by the server for a key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcmInfo {
    /// Raw key byte (key id plus the client offset).
    pub key: u8,
    /// Service id.
    pub sid: u16,
    /// CAID used.
    pub caid: u16,
    /// ECM PID.
    pub pid: u16,
    /// Provider id.
    pub prid: u32,
    /// ECM time in ms.
    pub ecm_time: i32,
    /// Card system name.
    pub cardsystem: Vec<u8>,
    /// Reader name.
    pub reader: Vec<u8>,
    /// Source address.
    pub from: Vec<u8>,
    /// Reader protocol.
    pub protocol: Vec<u8>,
    /// Hop count.
    pub hops: u8,
}

/// One message received from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerMsg {
    /// Handshake reply.
    ServerInfo {
        /// Server protocol version.
        version: u16,
        /// Server description, e.g. `OSCam v1.20, build r11720 (...)`.
        info: Vec<u8>,
    },
    /// Start a section filter.
    SetFilter {
        /// Raw key byte.
        key: u8,
        /// Server demux index.
        demux: u8,
        /// Server filter number.
        filter: u8,
        /// PID (13 bits).
        pid: u16,
        /// Filter bytes.
        data: [u8; 16],
        /// Mask bytes.
        mask: [u8; 16],
    },
    /// Stop a section filter.
    DmxStop {
        /// Raw key byte.
        key: u8,
        /// Server demux index.
        demux: u8,
        /// Server filter number.
        filter: u8,
        /// PID (13 bits).
        pid: u16,
    },
    /// PID to descrambler index assignment (ignored, like C++).
    CaSetPid {
        /// Raw key byte.
        key: u8,
    },
    /// 8-byte control word.
    CaSetDescr {
        /// Raw key byte.
        key: u8,
        /// Descrambler index.
        index: i32,
        /// Parity (0 even, 1 odd; others are invalid).
        parity: i32,
        /// Control word.
        cw: [u8; 8],
    },
    /// 16-byte AES control word.
    CaSetDescrAes {
        /// Raw key byte.
        key: u8,
        /// Descrambler index.
        index: i32,
        /// Parity.
        parity: i32,
        /// Control word.
        cw: [u8; 16],
    },
    /// Algorithm and cipher mode for the next control words.
    CaSetDescrMode {
        /// Raw key byte.
        key: u8,
        /// Descrambler index.
        index: i32,
        /// `CW_ALGO_*`.
        algo: i32,
        /// `CW_ALGO_MODE_*`.
        mode: i32,
    },
    /// ECM statistics.
    EcmInfo(EcmInfo),
    /// Unknown opcode: like C++, the rest of the buffered data is dropped.
    Unknown(Vec<u8>),
}

/// Result of [`parse_message`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// A full message and the bytes it used.
    Message(ServerMsg, usize),
    /// More bytes are needed.
    Incomplete,
}

struct Reader<'a> {
    b: &'a [u8],
    little_endian: bool,
}

impl Reader<'_> {
    fn u16(&self, i: usize) -> u16 {
        let v = [self.b[i], self.b[i + 1]];
        if self.little_endian {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        }
    }

    fn i32(&self, i: usize) -> i32 {
        let v = [self.b[i], self.b[i + 1], self.b[i + 2], self.b[i + 3]];
        if self.little_endian {
            i32::from_le_bytes(v)
        } else {
            i32::from_be_bytes(v)
        }
    }
}

fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Decodes the opcode like `dvbapi_reply`, undoing the key-first host order.
pub fn decode_opcode(b: &[u8]) -> (u32, bool) {
    let op = be32(b, 0);
    let op1 = op & 0xFF_FFFF;
    if matches!(
        op1,
        CA_SET_DESCR_X | CA_SET_DESCR_AES_X | CA_SET_PID_X | DMX_STOP_X | DMX_SET_FILTER_X
    ) {
        let mut swapped =
            0x4000_0000 | ((op1 & 0xFF) << 16) | (op1 & 0xFF00) | ((op1 & 0xFF_0000) >> 16);
        if swapped & 0xFF_0000 == 0 {
            swapped &= 0xFF_FFFF;
        }
        return (swapped, true);
    }
    (op, false)
}

fn array<const N: usize>(b: &[u8], at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&b[at..at + N]);
    out
}

/// Parses the first message in `b`.
pub fn parse_message(b: &[u8]) -> Parsed {
    if b.len() < 4 {
        return Parsed::Incomplete;
    }
    let (op, little_endian) = decode_opcode(b);
    let r = Reader { b, little_endian };
    let need = |n: usize| b.len() >= n;
    // In host order the key byte comes first and the opcode follows it.
    let key = |b: &[u8]| if little_endian { b[0] } else { b[4] };
    let msg = match op {
        DVBAPI_SERVER_INFO => {
            if !need(7) || !need(7 + b[6] as usize) {
                return Parsed::Incomplete;
            }
            let len = 7 + b[6] as usize;
            let info = b[7..len].to_vec();
            let version = u16::from_be_bytes([b[4], b[5]]);
            return Parsed::Message(ServerMsg::ServerInfo { version, info }, len);
        }
        DVBAPI_DMX_SET_FILTER => {
            let len = if little_endian { 67 } else { 65 };
            if !need(len) {
                return Parsed::Incomplete;
            }
            let msg = ServerMsg::SetFilter {
                key: key(b),
                demux: b[5],
                filter: b[6],
                pid: r.u16(7) & 0x1FFF,
                data: array(b, 9),
                mask: array(b, 25),
            };
            (msg, len)
        }
        DVBAPI_DMX_STOP => {
            if !need(9) {
                return Parsed::Incomplete;
            }
            let msg = ServerMsg::DmxStop {
                key: key(b),
                demux: b[5],
                filter: b[6],
                pid: r.u16(7) & 0x1FFF,
            };
            (msg, 9)
        }
        DVBAPI_CA_SET_PID => {
            if !need(13) {
                return Parsed::Incomplete;
            }
            (ServerMsg::CaSetPid { key: key(b) }, 13)
        }
        DVBAPI_CA_SET_DESCR => {
            if !need(21) {
                return Parsed::Incomplete;
            }
            let msg = ServerMsg::CaSetDescr {
                key: key(b),
                index: r.i32(5),
                parity: r.i32(9),
                cw: array(b, 13),
            };
            (msg, 21)
        }
        DVBAPI_CA_SET_DESCR_AES => {
            if !need(29) {
                return Parsed::Incomplete;
            }
            let msg = ServerMsg::CaSetDescrAes {
                key: key(b),
                index: r.i32(5),
                parity: r.i32(9),
                cw: array(b, 13),
            };
            (msg, 29)
        }
        DVBAPI_CA_SET_DESCR_MODE => {
            if !need(17) {
                return Parsed::Incomplete;
            }
            let msg = ServerMsg::CaSetDescrMode {
                key: key(b),
                index: r.i32(5),
                algo: r.i32(9),
                mode: r.i32(13),
            };
            (msg, 17)
        }
        DVBAPI_ECM_INFO => match parse_ecm_info(b) {
            Some((info, len)) => (ServerMsg::EcmInfo(info), len),
            None => return Parsed::Incomplete,
        },
        _ => (ServerMsg::Unknown(b.to_vec()), b.len()),
    };
    Parsed::Message(msg.0, msg.1)
}

fn parse_ecm_info(b: &[u8]) -> Option<(EcmInfo, usize)> {
    if b.len() < 19 {
        return None;
    }
    let mut info = EcmInfo {
        key: b[4],
        sid: u16::from_be_bytes([b[5], b[6]]),
        caid: u16::from_be_bytes([b[7], b[8]]),
        pid: u16::from_be_bytes([b[9], b[10]]),
        prid: be32(b, 11),
        ecm_time: be32(b, 15) as i32,
        ..EcmInfo::default()
    };
    let mut i = 19;
    for field in [
        &mut info.cardsystem,
        &mut info.reader,
        &mut info.from,
        &mut info.protocol,
    ] {
        let len = *b.get(i)? as usize;
        let text = b.get(i + 1..i + 1 + len)?;
        *field = text[..len.min(ECM_INFO_STR_MAX)].to_vec();
        i += 1 + len;
    }
    info.hops = *b.get(i)?;
    Some((info, i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_info_bytes() {
        assert_eq!(
            client_info(2, "minisatip/1.0"),
            b"\xFF\xFF\x00\x01\x00\x02\x0Dminisatip/1.0"
        );
    }

    #[test]
    fn host_order_opcodes_are_swapped() {
        let descr = [0x00, 0x86, 0x6F, 0x10, 0x40];
        assert_eq!(decode_opcode(&descr), (DVBAPI_CA_SET_DESCR, true));
        let stop = [0x00, 0x2A, 0x6F, 0x00, 0x00];
        assert_eq!(decode_opcode(&stop), (DVBAPI_DMX_STOP, true));
        let pid = [0x01, 0x87, 0x6F, 0x08, 0x40];
        assert_eq!(decode_opcode(&pid), (DVBAPI_CA_SET_PID, true));
        let aes = [0x01, 0x87, 0x6F, 0x10, 0x40];
        assert_eq!(decode_opcode(&aes), (DVBAPI_CA_SET_DESCR_AES, true));
        let filter = [0x01, 0x2B, 0x6F, 0x3C, 0x40];
        assert_eq!(decode_opcode(&filter), (DVBAPI_DMX_SET_FILTER, true));
        let be = DVBAPI_CA_SET_DESCR.to_be_bytes();
        assert_eq!(decode_opcode(&be), (DVBAPI_CA_SET_DESCR, false));
    }

    #[test]
    fn host_order_descr_reads_little_endian_fields() {
        let mut b = vec![0x03, 0x86, 0x6F, 0x10, 0x40];
        b.extend_from_slice(&7i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 6, 4, 5, 6, 15]);
        let msg = ServerMsg::CaSetDescr {
            key: 3,
            index: 7,
            parity: 1,
            cw: [1, 2, 3, 6, 4, 5, 6, 15],
        };
        assert_eq!(parse_message(&b), Parsed::Message(msg, 21));
        assert_eq!(parse_message(&b[..20]), Parsed::Incomplete);
    }

    #[test]
    fn host_order_set_filter_is_two_bytes_longer() {
        let mut b = vec![0u8; 67];
        b[..5].copy_from_slice(&[0x02, 0x2B, 0x6F, 0x3C, 0x40]);
        b[5] = 1;
        b[6] = 2;
        b[7..9].copy_from_slice(&0x0500u16.to_le_bytes());
        b[9] = 0x80;
        b[25] = 0xF0;
        let Parsed::Message(ServerMsg::SetFilter { key, pid, .. }, len) = parse_message(&b) else {
            panic!("no filter");
        };
        assert_eq!((key, pid, len), (2, 0x500, 67));
        assert_eq!(parse_message(&b[..66]), Parsed::Incomplete);
    }

    #[test]
    fn ecm_info_cuts_long_strings_but_skips_them_whole() {
        let mut b = DVBAPI_ECM_INFO.to_be_bytes().to_vec();
        b.push(5);
        b.extend_from_slice(&[0x00, 0x64, 0x09, 0x8C, 0x06, 0x00]);
        b.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 150]);
        b.push(70);
        b.extend_from_slice(&[b'x'; 70]);
        for s in [&b"reader"[..], b"from", b"proto"] {
            b.push(s.len() as u8);
            b.extend_from_slice(s);
        }
        b.push(2);
        let Parsed::Message(ServerMsg::EcmInfo(info), len) = parse_message(&b) else {
            panic!("no ECM_INFO");
        };
        assert_eq!(len, b.len());
        assert_eq!(info.cardsystem.len(), ECM_INFO_STR_MAX);
        assert_eq!(info.reader, b"reader");
        assert_eq!(info.protocol, b"proto");
        assert_eq!((info.sid, info.caid, info.pid), (0x64, 0x098C, 0x600));
        assert_eq!((info.prid, info.ecm_time, info.hops), (1, 150, 2));
        assert_eq!(parse_message(&b[..b.len() - 1]), Parsed::Incomplete);
    }

    #[test]
    fn unknown_opcode_takes_everything() {
        let b = [0x12, 0x34, 0x56, 0x78, 0x9A];
        assert_eq!(
            parse_message(&b),
            Parsed::Message(ServerMsg::Unknown(b.to_vec()), 5)
        );
    }
}
