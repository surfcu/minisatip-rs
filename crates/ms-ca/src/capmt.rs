//! CA_PMT building exactly as `create_capmt`/`CAPMT_add_PMT` in ca.cpp.

use crate::program::{Program, ProgramTable};
use ms_ts::crc::crc32_mpeg2;

pub const CLM_MORE: u8 = 0x00;
pub const CLM_FIRST: u8 = 0x01;
pub const CLM_LAST: u8 = 0x02;
pub const CLM_ONLY: u8 = 0x03;
pub const CLM_ADD: u8 = 0x04;
pub const CLM_UPDATE: u8 = 0x05;

pub const CMD_ID_OK_DESCRAMBLING: u8 = 0x01;
pub const CMD_ID_OK_MMI: u8 = 0x02;
pub const CMD_ID_QUERY: u8 = 0x03;
pub const CMD_ID_NOT_SELECTED: u8 = 0x04;

pub const MAX_CA_PMT: usize = 8;
pub const MAX_PMT_PER_CAPMT: usize = 3;
pub const CAPMT_KEPT_ES_MAX: usize = 1024;
pub const PMT_INVALID: i32 = -1;

/// `PMT_ID_IS_VALID`.
pub fn pmt_id_is_valid(id: i32) -> bool {
    id > PMT_INVALID
}

/// `MAKE_SID_FOR_CA`.
pub fn make_sid_for_ca(id: i32, idx: i32) -> i32 {
    id * 100 + idx
}

/// One CAPMT slot of a CAM (`SCAPMT`); place 0 is `pmt_id`, the rest `other_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capmt {
    pub pmt_id: i32,
    pub other_id: [i32; MAX_PMT_PER_CAPMT - 1],
    pub version: i32,
    pub sid: i32,
    pub capmt_hash: u32,
    pub capmt_hash_valid: bool,
    /// Stream loops last sent per place; empty means nothing kept.
    pub sent_es: [Vec<u8>; MAX_PMT_PER_CAPMT],
}

impl Default for Capmt {
    /// The `memset(capmt, -1)` state; the hash reads as valid `0xFFFFFFFF` like in C++.
    fn default() -> Self {
        Self {
            pmt_id: PMT_INVALID,
            other_id: [PMT_INVALID; MAX_PMT_PER_CAPMT - 1],
            version: -1,
            sid: -1,
            capmt_hash: u32::MAX,
            capmt_hash_valid: true,
            sent_es: Default::default(),
        }
    }
}

impl Capmt {
    /// A slot carrying `pmt_id` first and `others` packed, otherwise zeroed like a test literal.
    pub fn with_programs(
        pmt_id: i32,
        others: [i32; MAX_PMT_PER_CAPMT - 1],
        version: i32,
        sid: i32,
    ) -> Self {
        Self {
            pmt_id,
            other_id: others,
            version,
            sid,
            capmt_hash: 0,
            capmt_hash_valid: false,
            sent_es: Default::default(),
        }
    }

    pub fn place(&self, place: usize) -> i32 {
        if place == 0 {
            self.pmt_id
        } else {
            self.other_id[place - 1]
        }
    }

    pub fn place_mut(&mut self, place: usize) -> &mut i32 {
        if place == 0 {
            &mut self.pmt_id
        } else {
            &mut self.other_id[place - 1]
        }
    }

    /// `capmt_has_pmt`.
    pub fn has_pmt(&self, pmt_id: i32) -> bool {
        pmt_id_is_valid(pmt_id) && (0..MAX_PMT_PER_CAPMT).any(|p| self.place(p) == pmt_id)
    }

    /// `capmt_pmt_count`.
    pub fn pmt_count(&self) -> usize {
        (0..MAX_PMT_PER_CAPMT)
            .filter(|&p| pmt_id_is_valid(self.place(p)))
            .count()
    }

    /// `capmt_first_pmt_id`: place 0 may be empty after a lazy leave.
    pub fn first_pmt_id(&self) -> i32 {
        (0..MAX_PMT_PER_CAPMT)
            .map(|p| self.place(p))
            .find(|&id| pmt_id_is_valid(id))
            .unwrap_or(PMT_INVALID)
    }

    /// `capmt_forget_sent`.
    pub fn forget_sent(&mut self) {
        for es in &mut self.sent_es {
            es.clear();
        }
    }

    /// `capmt_kept_count`: left places whose streams are kept for a rejoin.
    pub fn kept_count(&self) -> usize {
        (0..MAX_PMT_PER_CAPMT)
            .filter(|&p| !pmt_id_is_valid(self.place(p)) && !self.sent_es[p].is_empty())
            .count()
    }

    /// `capmt_compact`: programs to the front.
    pub fn compact(&mut self) {
        let ids: Vec<i32> = (0..MAX_PMT_PER_CAPMT)
            .map(|p| self.place(p))
            .filter(|&id| pmt_id_is_valid(id))
            .collect();
        for p in 0..MAX_PMT_PER_CAPMT {
            *self.place_mut(p) = ids.get(p).copied().unwrap_or(PMT_INVALID);
        }
    }

    /// PMT ids for logs, e.g. "3 7 -1" (`capmt_pmts_str`).
    pub fn pmts_str(&self) -> String {
        let mut s = self.pmt_id.to_string();
        for id in self.other_id {
            s.push(' ');
            s.push_str(&id.to_string());
        }
        s
    }
}

/// Which CAIDs a CA may handle (`match_ca_caid`); empty accepts every CAID.
pub fn match_ca_caid(filter: &[(u16, u16)], caid: u16) -> bool {
    filter.is_empty() || filter.iter().any(|&(c, mask)| c == caid & mask)
}

/// `pmt_add_ca_descriptor`: the program CA descriptors fitting in `buf_len`.
pub fn pmt_add_ca_descriptor(
    pmt: &Program,
    out: &mut Vec<u8>,
    buf_len: usize,
    filter: &[(u16, u16)],
) -> usize {
    let mut len = 0;
    for ca in &pmt.caids {
        if !match_ca_caid(filter, ca.caid) {
            continue;
        }
        let private = ca.private_data.len();
        if len + 6 + private > buf_len {
            break;
        }
        out.push(0x09);
        out.push((4 + private) as u8);
        out.extend_from_slice(&ca.caid.to_be_bytes());
        out.extend_from_slice(&ca.pid.to_be_bytes());
        out.extend_from_slice(&ca.private_data);
        len += 6 + private;
    }
    len
}

/// Inputs shared by every CAPMT build of one CA.
#[derive(Clone, Copy)]
pub struct BuildCtx<'a> {
    pub programs: &'a ProgramTable,
    /// CAIDs this CA handles for the program's adapter.
    pub caid_filter: &'a [(u16, u16)],
}

/// `CAPMT_add_PMT`: the stream loop of one program, at most `len` bytes.
pub fn capmt_add_pmt(
    out: &mut Vec<u8>,
    len: usize,
    pmt: &Program,
    cmd_id: u8,
    added_only: bool,
    ctx: BuildCtx<'_>,
) -> usize {
    let start = out.len();
    for es in &pmt.streams {
        if added_only && !ctx.programs.pid_requested(pmt.adapter, es.pid) {
            continue;
        }
        if !es.is_audio && !es.is_video {
            continue;
        }
        let pos = out.len() - start;
        if pos + 6 > len {
            break;
        }
        out.push(es.stream_type);
        out.extend_from_slice(&es.pid.to_be_bytes());
        let pi_len_pos = out.len();
        out.extend_from_slice(&[0, 0]);
        if !pmt.caids.is_empty() {
            out.push(cmd_id);
            let room = len - (out.len() - start);
            let pi_len = pmt_add_ca_descriptor(pmt, out, room, ctx.caid_filter) + 1;
            out[pi_len_pos..pi_len_pos + 2].copy_from_slice(&(pi_len as u16).to_be_bytes());
        }
    }
    out.len() - start
}

/// Why `create_capmt` built nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapmtError {
    /// The first program of the slot does not exist (C++ returns 1).
    PmtNotFound(i32),
}

/// A built CAPMT body and the `(offset, len)` of each place's stream loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltCapmt {
    pub bytes: Vec<u8>,
    pub spans: [(usize, usize); MAX_PMT_PER_CAPMT],
}

/// `create_capmt`; with `kept`, an empty place repeats the streams last sent there.
#[allow(clippy::too_many_arguments)]
pub fn create_capmt(
    ca: &Capmt,
    listmgmt: u8,
    capmt_len: usize,
    cmd_id: u8,
    added_only: bool,
    kept: bool,
    ctx: BuildCtx<'_>,
) -> Result<BuiltCapmt, CapmtError> {
    let first = ca.first_pmt_id();
    if ctx.programs.get(first).is_none() {
        return Err(CapmtError::PmtNotFound(first));
    }
    let mut out = Vec::with_capacity(256);
    out.push(listmgmt);
    out.extend_from_slice(&(ca.sid as u16).to_be_bytes());
    out.push((((ca.version & 0xF) << 1) | 1) as u8);
    out.extend_from_slice(&[0, 0]);
    let mut spans = [(0, 0); MAX_PMT_PER_CAPMT];
    for (place, span) in spans.iter_mut().enumerate() {
        let start = out.len();
        let id = ca.place(place);
        let kept_es = &ca.sent_es[place];
        if let Some(pmt) = ctx.programs.get(id) {
            let room = capmt_len.saturating_sub(start);
            capmt_add_pmt(&mut out, room, pmt, cmd_id, added_only, ctx);
        } else if kept
            && !pmt_id_is_valid(id)
            && !kept_es.is_empty()
            && kept_es.len() <= CAPMT_KEPT_ES_MAX
            && start + kept_es.len() <= capmt_len
        {
            out.extend_from_slice(kept_es);
        }
        *span = (start, out.len() - start);
    }
    Ok(BuiltCapmt { bytes: out, spans })
}

/// `capmt_content_hash`: CRC of the CAPMT with version and list management masked.
pub fn capmt_content_hash(capmt: &[u8]) -> u32 {
    if capmt.len() <= 4 {
        return crc32_mpeg2(capmt);
    }
    let mut masked = capmt.to_vec();
    masked[0] = 0;
    masked[3] &= !0x3E;
    crc32_mpeg2(&masked)
}
