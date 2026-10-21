//! CA_PMT and CA_PMT_REPLY bodies (EN 50221 §8.4.3.4/5), laid out like `create_capmt`.

use std::fmt;

/// `ca_pmt_list_management` values (`CLM_*` in pmt.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListManagement {
    More,
    First,
    Last,
    Only,
    Add,
    Update,
    Other(u8),
}

impl ListManagement {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::More,
            1 => Self::First,
            2 => Self::Last,
            3 => Self::Only,
            4 => Self::Add,
            5 => Self::Update,
            v => Self::Other(v),
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            Self::More => 0,
            Self::First => 1,
            Self::Last => 2,
            Self::Only => 3,
            Self::Add => 4,
            Self::Update => 5,
            Self::Other(v) => v,
        }
    }
}

impl fmt::Display for ListManagement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::More => f.write_str("more"),
            Self::First => f.write_str("first"),
            Self::Last => f.write_str("last"),
            Self::Only => f.write_str("only"),
            Self::Add => f.write_str("add"),
            Self::Update => f.write_str("update"),
            Self::Other(v) => write!(f, "lm{v:02X}"),
        }
    }
}

pub const CMD_OK_DESCRAMBLING: u8 = 0x01;
pub const CMD_OK_MMI: u8 = 0x02;
pub const CMD_QUERY: u8 = 0x03;
pub const CMD_NOT_SELECTED: u8 = 0x04;

/// `CA_enable` value: descrambling possible.
pub const CA_ENABLE_POSSIBLE: u8 = 0x01;
/// `CA_enable` value: descrambling not possible for technical reasons.
pub const CA_ENABLE_NOT_POSSIBLE_TECHNICAL: u8 = 0x73;

/// One elementary stream entry of a CA_PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaPmtStream {
    pub stream_type: u8,
    pub pid: u16,
    pub cmd_id: Option<u8>,
    /// Raw CA descriptors following `ca_pmt_cmd_id`.
    pub descriptors: Vec<u8>,
}

/// A decoded CA_PMT APDU body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaPmt {
    pub list_management: ListManagement,
    pub program_number: u16,
    pub version: u8,
    pub current_next: bool,
    pub cmd_id: Option<u8>,
    pub descriptors: Vec<u8>,
    pub streams: Vec<CaPmtStream>,
}

/// Why a CA_PMT body could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaPmtError {
    Truncated,
}

fn info_block(data: &[u8], at: usize) -> Result<(Option<u8>, Vec<u8>, usize), CaPmtError> {
    let hdr = data.get(at..at + 2).ok_or(CaPmtError::Truncated)?;
    let len = (usize::from(hdr[0] & 0x0F) << 8) | usize::from(hdr[1]);
    let body = data
        .get(at + 2..at + 2 + len)
        .ok_or(CaPmtError::Truncated)?;
    let (cmd, desc) = match body.split_first() {
        Some((c, d)) => (Some(*c), d.to_vec()),
        None => (None, Vec::new()),
    };
    Ok((cmd, desc, at + 2 + len))
}

fn push_info(out: &mut Vec<u8>, cmd: Option<u8>, desc: &[u8]) {
    let len = cmd.map_or(0, |_| 1 + desc.len());
    out.extend_from_slice(&[(len >> 8) as u8, len as u8]);
    if let Some(c) = cmd {
        out.push(c);
        out.extend_from_slice(desc);
    }
}

impl CaPmt {
    /// Decodes a CA_PMT body (the APDU payload after tag and length).
    pub fn parse(data: &[u8]) -> Result<Self, CaPmtError> {
        let hdr = data.get(..4).ok_or(CaPmtError::Truncated)?;
        let (cmd_id, descriptors, mut pos) = info_block(data, 4)?;
        let mut streams = Vec::new();
        while pos < data.len() {
            let es = data.get(pos..pos + 3).ok_or(CaPmtError::Truncated)?;
            let (cmd, desc, next) = info_block(data, pos + 3)?;
            streams.push(CaPmtStream {
                stream_type: es[0],
                pid: (u16::from(es[1] & 0x1F) << 8) | u16::from(es[2]),
                cmd_id: cmd,
                descriptors: desc,
            });
            pos = next;
        }
        Ok(Self {
            list_management: ListManagement::from_byte(hdr[0]),
            program_number: u16::from_be_bytes([hdr[1], hdr[2]]),
            version: (hdr[3] >> 1) & 0x1F,
            current_next: hdr[3] & 1 == 1,
            cmd_id,
            descriptors,
            streams,
        })
    }

    /// Encodes the body; with no program-level command, program_info_length is 0.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.list_management.to_byte()];
        out.extend_from_slice(&self.program_number.to_be_bytes());
        out.push(((self.version & 0x1F) << 1) | u8::from(self.current_next));
        push_info(&mut out, self.cmd_id, &self.descriptors);
        for s in &self.streams {
            out.push(s.stream_type);
            out.extend_from_slice(&s.pid.to_be_bytes());
            push_info(&mut out, s.cmd_id, &s.descriptors);
        }
        out
    }

    /// Command of a stream: its own, else the program-level one.
    pub fn stream_cmd(&self, stream: &CaPmtStream) -> Option<u8> {
        stream.cmd_id.or(self.cmd_id)
    }

    /// Command for the whole program: program level, else the first stream's.
    pub fn program_cmd(&self) -> Option<u8> {
        self.cmd_id
            .or_else(|| self.streams.iter().find_map(|s| s.cmd_id))
    }

    /// One-line summary used in the recorded log.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "lm={} pn={:04X} v={} cmd={}",
            self.list_management,
            self.program_number,
            self.version,
            self.program_cmd()
                .map_or_else(|| "-".to_owned(), |c| format!("{c:02X}"))
        );
        for es in &self.streams {
            s.push_str(&format!(" es={:04X}", es.pid));
            for (caid, pid) in ca_descriptors(&es.descriptors) {
                s.push_str(&format!("/{caid:04X}:{pid:04X}"));
            }
        }
        s
    }
}

/// `(CA_system_id, CA_PID)` of every well-formed CA descriptor (tag 0x09).
pub fn ca_descriptors(mut data: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    while let [tag, len, rest @ ..] = data {
        let len = usize::from(*len);
        let Some(body) = rest.get(..len) else { break };
        if *tag == 0x09 && len >= 4 {
            out.push((
                u16::from_be_bytes([body[0], body[1]]),
                u16::from_be_bytes([body[2] & 0x1F, body[3]]),
            ));
        }
        data = &rest[len..];
    }
    out
}

/// Encodes a CA descriptor the way `pmt_add_ca_descriptor` does.
pub fn ca_descriptor(caid: u16, pid: u16, private: &[u8]) -> Vec<u8> {
    let mut d = vec![0x09, 4 + private.len() as u8];
    d.extend_from_slice(&caid.to_be_bytes());
    d.extend_from_slice(&pid.to_be_bytes());
    d.extend_from_slice(private);
    d
}

/// Encodes a CA_PMT_REPLY body with one `CA_enable` for the program and each stream.
pub fn encode_reply(pmt: &CaPmt, ca_enable: u8) -> Vec<u8> {
    let mut out = pmt.program_number.to_be_bytes().to_vec();
    out.push(0xC0 | ((pmt.version & 0x1F) << 1) | u8::from(pmt.current_next));
    out.push(0x80 | ca_enable);
    for s in &pmt.streams {
        out.extend_from_slice(&(0xE000 | s.pid).to_be_bytes());
        out.push(0x80 | ca_enable);
    }
    out
}

/// A decoded CA_PMT_REPLY body (EN 50221 §8.4.3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaPmtReply {
    pub program_number: u16,
    pub version: u8,
    pub current_next: bool,
    pub ca_enable: u8,
    /// `(elementary PID, CA_enable)` in list order.
    pub streams: Vec<(u16, u8)>,
}

impl CaPmtReply {
    /// Decodes a CA_PMT_REPLY body; `None` when shorter than the 4-byte header.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let hdr = data.get(..4)?;
        let mut streams = Vec::new();
        let mut pos = 4;
        while let Some(es) = data.get(pos..pos + 3) {
            streams.push((
                (u16::from(es[0] & 0x1F) << 8) | u16::from(es[1]),
                es[2] & 0x7F,
            ));
            pos += 3;
        }
        Some(Self {
            program_number: u16::from_be_bytes([hdr[0], hdr[1]]),
            version: (hdr[2] >> 1) & 0x1F,
            current_next: hdr[2] & 1 == 1,
            ca_enable: hdr[3] & 0x7F,
            streams,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(ty: u8, pid: u16, ca: Option<(u16, u16)>) -> CaPmtStream {
        CaPmtStream {
            stream_type: ty,
            pid,
            cmd_id: ca.map(|_| CMD_OK_DESCRAMBLING),
            descriptors: ca.map_or_else(Vec::new, |(c, p)| ca_descriptor(c, p, &[])),
        }
    }

    fn capmt(lm: ListManagement, streams: Vec<CaPmtStream>) -> CaPmt {
        CaPmt {
            list_management: lm,
            program_number: 0x1234,
            version: 1,
            current_next: true,
            cmd_id: None,
            descriptors: Vec::new(),
            streams,
        }
    }

    // From tests/test_ca.cpp:207 (test_create_capmt_single_clear)
    #[test]
    fn single_clear_layout() {
        let b = capmt(
            ListManagement::Only,
            vec![stream(2, 0x501, None), stream(3, 0x502, None)],
        )
        .encode();
        assert_eq!(b[0], 3);
        assert_eq!(u16::from_be_bytes([b[1], b[2]]), 0x1234);
        assert_eq!((b[3] & 0x3E) >> 1, 1);
        assert_eq!(b[3] & 1, 1);
        assert_eq!(&b[4..6], &[0, 0]);
        assert_eq!(b[6], 2);
        assert_eq!(b[11], 3);
    }

    // From tests/test_ca.cpp:294 (test_create_capmt_single_pmt_scrambled)
    #[test]
    fn scrambled_streams_carry_es_level_descriptors() {
        let ca = Some((0x0B00, 0x573));
        let b = capmt(
            ListManagement::Only,
            vec![stream(2, 0x501, ca), stream(3, 0x502, ca)],
        )
        .encode();
        assert_eq!(&b[4..6], &[0, 0]);
        assert_eq!(b[6], 2);
        assert_eq!(u16::from_be_bytes([b[9], b[10]]), 7);
        assert_eq!(b[18], 3);
        assert_eq!(u16::from_be_bytes([b[21], b[22]]), 7);
        let p = CaPmt::parse(&b).unwrap();
        assert_eq!(p.streams[0].cmd_id, Some(CMD_OK_DESCRAMBLING));
        assert_eq!(ca_descriptors(&p.streams[1].descriptors), [(0x0B00, 0x573)]);
    }

    // From tests/test_ca.cpp:393 (test_create_capmt_three_pmts)
    #[test]
    fn three_packed_programs_parse_in_order() {
        let mut streams = Vec::new();
        for i in 0..3u16 {
            let ca = Some((0x0664, 0x700 + i));
            streams.push(stream(2, 0x800 + i * 0x10, ca));
            streams.push(stream(3, 0x801 + i * 0x10, ca));
        }
        let b = capmt(ListManagement::Only, streams).encode();
        assert_eq!(b.len(), 6 + 6 * 12);
        let p = CaPmt::parse(&b).unwrap();
        for (i, s) in p.streams.iter().enumerate() {
            let i = i as u16;
            assert_eq!(s.pid, 0x800 + (i / 2) * 0x10 + i % 2);
            assert_eq!(ca_descriptors(&s.descriptors), [(0x0664, 0x700 + i / 2)]);
        }
    }

    // From tests/test_ca.cpp:468 (test_create_capmt_different_listmgmt)
    #[test]
    fn list_management_round_trips() {
        for lm in 0..=6u8 {
            let mut c = capmt(ListManagement::from_byte(lm), vec![stream(2, 0x501, None)]);
            c.version = 31;
            let p = CaPmt::parse(&c.encode()).unwrap();
            assert_eq!(p.list_management.to_byte(), lm);
            assert_eq!(p.version, 31);
            assert_eq!(p, c);
        }
    }

    // From tests/test_ca.cpp:566 (test_create_capmt_max_version)
    #[test]
    fn max_version_survives_the_header() {
        let mut c = capmt(ListManagement::Only, vec![stream(2, 0x501, None)]);
        c.version = 15;
        let b = c.encode();
        assert_eq!((b[3] & 0x3E) >> 1, 15);
    }

    // From tests/test_ca.cpp:514 (test_create_capmt_different_cmd_ids): the
    // command sits at offset 11, after stream type, PID and ES info length.
    #[test]
    fn command_id_lands_at_stream_offset_eleven() {
        for cmd in [CMD_OK_MMI, CMD_QUERY, CMD_NOT_SELECTED] {
            let mut c = capmt(
                ListManagement::Only,
                vec![stream(2, 0x501, Some((0x0B00, 0x573)))],
            );
            c.streams[0].cmd_id = Some(cmd);
            let b = c.encode();
            assert_eq!(b[11], cmd);
        }
    }

    #[test]
    fn truncated_bodies_are_rejected() {
        let b = capmt(
            ListManagement::Only,
            vec![stream(2, 0x501, Some((0x100, 0x200)))],
        )
        .encode();
        for cut in 0..b.len() {
            if cut != 6 {
                assert_eq!(CaPmt::parse(&b[..cut]), Err(CaPmtError::Truncated), "{cut}");
            }
        }
        assert!(CaPmt::parse(&b[..6]).unwrap().streams.is_empty());
    }

    #[test]
    fn reply_lists_program_and_streams() {
        let c = capmt(
            ListManagement::Only,
            vec![stream(2, 0x501, None), stream(3, 0x502, None)],
        );
        assert_eq!(
            encode_reply(&c, CA_ENABLE_POSSIBLE),
            [0x12, 0x34, 0xC3, 0x81, 0xE5, 0x01, 0x81, 0xE5, 0x02, 0x81]
        );
        let r = CaPmtReply::parse(&encode_reply(&c, CA_ENABLE_NOT_POSSIBLE_TECHNICAL)).unwrap();
        assert_eq!(r.program_number, 0x1234);
        assert_eq!(r.version, 1);
        assert!(r.current_next);
        assert_eq!(r.ca_enable, CA_ENABLE_NOT_POSSIBLE_TECHNICAL);
        assert_eq!(
            r.streams,
            [
                (0x501, CA_ENABLE_NOT_POSSIBLE_TECHNICAL),
                (0x502, CA_ENABLE_NOT_POSSIBLE_TECHNICAL)
            ]
        );
        assert_eq!(CaPmtReply::parse(&[0, 1, 2]), None);
    }
}
