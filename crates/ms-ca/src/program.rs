//! Program data the CA layer reads, mirroring the `SPMT` fields used by ca.cpp/ddci.cpp.

use std::collections::BTreeSet;

/// One program-level CA descriptor (`SPMTCA`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaDescriptor {
    pub caid: u16,
    pub pid: u16,
    pub private_data: Vec<u8>,
}

/// A raw ES descriptor (`descriptor_t`): tag and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawDescriptor {
    pub tag: u8,
    pub data: Vec<u8>,
}

impl RawDescriptor {
    /// Builds from `tag, len, body...` bytes (`create_descriptor`).
    pub fn from_bytes(b: &[u8]) -> Self {
        let len = usize::from(b[1]);
        Self {
            tag: b[0],
            data: b[2..2 + len].to_vec(),
        }
    }

    pub fn is_ca(&self) -> bool {
        self.tag == 0x09
    }
}

/// One elementary stream (`SStreamPid`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EsStream {
    pub stream_type: u8,
    pub pid: u16,
    pub is_audio: bool,
    pub is_video: bool,
    pub descriptors: Vec<RawDescriptor>,
}

/// Service data from the transponder SDT (`has_sdt` and the `sdt_*` fields).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SdtInfo {
    pub service_type: u8,
    pub running_status: u8,
    pub ca_mode: u8,
    pub eit_schedule: u8,
    pub eit_pf: u8,
    pub tsid: u16,
    pub onid: u16,
}

/// A program as seen by the CA layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub id: i32,
    pub adapter: u16,
    pub sid: u16,
    pub pid: u16,
    pub pcr_pid: u16,
    pub version: i32,
    pub caids: Vec<CaDescriptor>,
    pub streams: Vec<EsStream>,
    pub name: String,
    pub provider: String,
    pub sdt: Option<SdtInfo>,
}

impl Program {
    /// `pmt_add_stream_pid`.
    pub fn add_stream(&mut self, pid: u16, stream_type: u8, is_audio: bool, is_video: bool) {
        self.streams.push(EsStream {
            stream_type,
            pid,
            is_audio,
            is_video,
            descriptors: Vec::new(),
        });
    }

    /// `pmt_add_caid`: duplicates and entries past `MAX_CAID` are ignored.
    pub fn add_caid(&mut self, caid: u16, pid: u16, private_data: &[u8]) {
        if self.caids.iter().any(|c| c.caid == caid && c.pid == pid) || self.caids.len() >= MAX_CAID
        {
            return;
        }
        self.caids.push(CaDescriptor {
            caid,
            pid,
            private_data: private_data.to_vec(),
        });
    }

    /// Removes the first stream with this pid; true when one was removed.
    pub fn remove_stream(&mut self, pid: u16) -> bool {
        match self.streams.iter().position(|s| s.pid == pid) {
            Some(i) => {
                self.streams.remove(i);
                true
            }
            None => false,
        }
    }

    /// `match_caid`: any CAID of the program matches `caid` under `mask`.
    pub fn match_caid(&self, caid: u16, mask: u16) -> bool {
        self.caids.iter().any(|c| c.caid & mask == caid)
    }
}

/// `MAX_CAID` in pmt.h.
pub const MAX_CAID: usize = 20;

/// Program store with reused ids (`pmts[]`, `pmt_add`, `pmt_del`).
#[derive(Clone, Debug, Default)]
pub struct ProgramTable {
    slots: Vec<Option<Program>>,
    /// `(adapter, pid)` requested by clients; `None` means every pid is.
    requested: Option<BTreeSet<(u16, u16)>>,
}

impl ProgramTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// `pmt_add`: the lowest free id.
    pub fn add(&mut self, adapter: u16, sid: u16, pid: u16) -> i32 {
        let id = self
            .slots
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.slots.len());
        let program = Program {
            id: id as i32,
            adapter,
            sid,
            pid,
            version: -1,
            ..Program::default()
        };
        if id == self.slots.len() {
            self.slots.push(Some(program));
        } else {
            self.slots[id] = Some(program);
        }
        id as i32
    }

    /// `pmt_del`.
    pub fn del(&mut self, id: i32) {
        if let Some(slot) = usize::try_from(id).ok().and_then(|i| self.slots.get_mut(i)) {
            *slot = None;
        }
    }

    pub fn get(&self, id: i32) -> Option<&Program> {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.slots.get(i))
            .and_then(Option::as_ref)
    }

    pub fn get_mut(&mut self, id: i32) -> Option<&mut Program> {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.slots.get_mut(i))
            .and_then(Option::as_mut)
    }

    /// `get_all_pmt_for_sid`: the first program on `adapter` with `sid`.
    pub fn first_for_sid(&self, adapter: u16, sid: u16) -> Option<&Program> {
        self.iter().find(|p| p.adapter == adapter && p.sid == sid)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Program> {
        self.slots.iter().flatten()
    }

    /// Tracks requested pids from now on (adapter pid table, `find_pid`).
    pub fn track_requested_pids(&mut self) {
        self.requested.get_or_insert_with(BTreeSet::new);
    }

    pub fn request_pid(&mut self, adapter: u16, pid: u16) {
        self.requested
            .get_or_insert_with(BTreeSet::new)
            .insert((adapter, pid));
    }

    pub fn release_pid(&mut self, adapter: u16, pid: u16) {
        if let Some(set) = self.requested.as_mut() {
            set.remove(&(adapter, pid));
        }
    }

    /// `find_pid(adapter, pid) != NULL` for the CAPMT `added_only` filter.
    pub fn pid_requested(&self, adapter: u16, pid: u16) -> bool {
        self.requested
            .as_ref()
            .is_none_or(|s| s.contains(&(adapter, pid)))
    }
}
