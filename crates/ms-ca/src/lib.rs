//! CAPMT building and the driver-facing interface for CA backends.
//! CAMs get one PMT per [`CaDevice::process_pmt`]/[`CaDevice::del_pmt`] call; DVBAPI gets a [`CapmtDiff`] via [`CaDriver::apply`].

pub mod capmt;
pub mod device;
pub mod program;

pub use capmt::{BuiltCapmt, Capmt, CapmtError, capmt_content_hash, create_capmt};
pub use device::{CaDevice, CaState, SendCmd, SendOutcome};
pub use program::{CaDescriptor, EsStream, Program, ProgramTable, RawDescriptor, SdtInfo};

/// Most CA descriptors kept per service, like the C++ `MAX_CAID`.
pub const MAX_CAID: usize = 20;

/// One elementary stream of a service as listed in a CAPMT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamPid {
    /// PMT stream type (0 for the independent PCR entry).
    pub stream_type: u8,
    /// Elementary PID.
    pub pid: u16,
}

/// A service a CA driver is asked to descramble (the C++ `SPMT` view).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaService {
    /// PMT slot id; identifies the service in every op and event.
    pub pmt_id: usize,
    /// Logical adapter id.
    pub adapter: usize,
    /// Physical adapter number (`ad->pa`), used by local-socket DVBAPI.
    pub hw_adapter: u8,
    /// Frontend/demux number (`ad->fn`), used by local-socket DVBAPI.
    pub hw_demux: u8,
    /// Transport stream id of the tuned mux.
    pub tsid: u16,
    /// Service id (program number).
    pub sid: u16,
    /// PMT PID.
    pub pmt_pid: u16,
    /// PCR PID from the section header (`SPMT::pcr_pid`).
    pub pcr_pid: u16,
    /// CA descriptors, deduplicated by (CAID, PID) like `pmt_add_caid`.
    pub ca: Vec<CaDescriptor>,
    /// Elementary streams in PMT order, plus an independent PCR entry.
    pub streams: Vec<StreamPid>,
    /// Streams the C++ parser classifies as audio or video (CAPMT ES filter).
    pub av_pids: Vec<u16>,
    /// Service name from the SDT (`SPMT::name`), empty until it arrives.
    pub name: String,
    /// Provider name from the SDT (`SPMT::provider`).
    pub provider: String,
    /// SDT service fields (`has_sdt` and `sdt_*`), `None` until the SDT arrives.
    pub sdt: Option<SdtInfo>,
}

impl CaService {
    /// Builds the service from a parsed PMT with the C++ `process_pmt` rules.
    pub fn from_pmt(pmt_id: usize, adapter: usize, pmt_pid: u16, pmt: &ms_psi::Pmt) -> Self {
        let mut service = Self {
            pmt_id,
            adapter,
            sid: pmt.header.table_id_extension,
            pmt_pid,
            pcr_pid: pmt.pcr_pid_header,
            ..Self::default()
        };
        let es = pmt.streams.iter().flat_map(|s| s.descriptors.iter());
        for descriptor in pmt.program_info.iter().chain(es) {
            if descriptor.is_ca() {
                service.add_ca_descriptor(&descriptor.data);
            }
        }
        service.streams = pmt
            .streams
            .iter()
            .map(|s| StreamPid {
                stream_type: s.stream_type,
                pid: s.elementary_pid,
            })
            .collect();
        service.av_pids = pmt
            .streams
            .iter()
            .filter(|s| s.is_audio || s.is_video)
            .map(|s| s.elementary_pid)
            .collect();
        service
    }

    /// Adds a CA descriptor payload unless its (CAID, PID) already exists.
    pub fn add_ca_descriptor(&mut self, data: &[u8]) {
        if data.len() < 4 || self.ca.len() >= MAX_CAID {
            return;
        }
        let caid = u16::from_be_bytes([data[0], data[1]]);
        let pid = (((data[2] & 0x1F) as u16) << 8) | data[3] as u16;
        if self.ca.iter().any(|c| c.caid == caid && c.pid == pid) {
            return;
        }
        self.ca.push(CaDescriptor {
            caid,
            pid,
            private_data: data[4..].to_vec(),
        });
    }
}

/// One change to the set of services a driver descrambles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapmtOp {
    /// Start descrambling a service (C++ `ca_add_pmt` with `update = 0`).
    Add(CaService),
    /// Re-send a changed service (C++ `ca_add_pmt` with `update = 1`).
    Update(CaService),
    /// Stop descrambling a service (C++ `ca_del_pmt`).
    Remove {
        /// PMT slot id of the service.
        pmt_id: usize,
    },
}

/// Ordered list of ops a network backend applies in [`CaDriver::apply`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapmtDiff {
    /// Ops applied in order.
    pub ops: Vec<CapmtOp>,
}

/// `TABLES_RESULT_*` returned by a CA when a program is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TablesResult {
    Ok,
    Retry,
    NoRetry,
}

/// Static limits and capabilities of a CA.
///
/// The CAM path uses `max_ca_pmt`/`multiple_pmt` (PLAN §4 `CaCaps`); a network
/// driver such as DVBAPI reports its descrambling slots in `max_services`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaCaps {
    /// Most services descrambled at once, `None` when unbounded.
    pub max_services: Option<usize>,
    pub max_ca_pmt: usize,
    pub multiple_pmt: usize,
}

/// Descrambling algorithm of a control word (C++ `CA_ALGO_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CwAlgo {
    /// DVB-CSA (`CA_ALGO_DVBCSA`).
    #[default]
    Csa,
    /// DES (`CA_ALGO_DES`).
    Des,
    /// AES-128 ECB (`CA_ALGO_AES128_ECB`).
    Aes128Ecb,
    /// AES-128 CBC (`CA_ALGO_AES128_CBC`).
    Aes128Cbc,
    /// DVB-CSA with the VideoGuard iCAM ECM mode (`CA_ALGO_DVBCSA_ICAM`).
    CsaIcam,
}

/// Opaque id of a section filter a driver asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FilterId(pub u32);

/// Events a driver reports back to the control plane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaEvent {
    /// The driver can take services (DVBAPI: registered after SERVER_INFO).
    Ready,
    /// The CA is gone (`ca_del_pmt` removed the last program).
    Gone,
    /// The server is too old and was not registered.
    Unsupported {
        /// Server build number.
        build: i32,
    },
    /// The driver lost its CA device; every service is gone.
    Closed,
    /// A control word for a service.
    ControlWord {
        /// PMT slot id of the service.
        pmt_id: usize,
        /// Descrambling algorithm.
        algo: CwAlgo,
        /// 0 = even, 1 = odd.
        parity: u8,
        /// Key bytes (8 for CSA/DES, 8 or 16 for AES).
        cw: Vec<u8>,
        /// iCAM ECM mode byte at the time of the CW.
        icam_ecm: u8,
    },
    /// Deliver sections of `pid` matching `filter`/`mask` via the driver.
    SetFilter {
        /// Filter id to pass back with each section.
        id: FilterId,
        /// Index of the filter in the driver's key table.
        slot: usize,
        /// PMT slot id of the service owning the filter.
        pmt_id: usize,
        /// Adapter the PID is read from.
        adapter: usize,
        /// Filtered PID.
        pid: u16,
        /// Filter bytes.
        filter: [u8; 16],
        /// Mask bytes.
        mask: [u8; 16],
        /// Whether this is an EMM filter.
        emm: bool,
    },
    /// Stop a filter set before.
    StopFilter {
        /// Filter id from `SetFilter`.
        id: FilterId,
    },
    /// The driver dropped a service; the scheduler should send it again.
    Restart {
        /// PMT slot id of the service.
        pmt_id: usize,
    },
}

/// Errors from [`CaDriver::apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaError {
    /// The CA device is not connected; retry later.
    NotConnected,
    /// No free descrambling slot; retry later.
    NoSlot,
}

impl std::fmt::Display for CaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => f.write_str("CA device not connected"),
            Self::NoSlot => f.write_str("no free descrambling slot"),
        }
    }
}

impl std::error::Error for CaError {}

/// A CA backend, driven by per-PMT `process_pmt`/`del_pmt` calls or by a CAPMT diff.
///
/// CAM transports implement `has_ca_session`/`send_ca_pmt`; network drivers
/// implement `apply` and report events. Methods a backend does not use keep
/// their defaults.
pub trait CaDriver {
    /// Static capabilities.
    fn capabilities(&self) -> CaCaps;
    /// A CA support session is open (`find_session_for_resource`).
    fn has_ca_session(&self) -> bool {
        false
    }
    /// Writes one CA_PMT APDU body; false when the write failed.
    fn send_ca_pmt(&mut self, _body: &[u8]) -> bool {
        false
    }
    /// Applies `diff` in order; stops at the first error.
    fn apply(&mut self, _diff: &CapmtDiff) -> Result<(), CaError> {
        Ok(())
    }
    /// Next pending event, if any.
    fn events(&mut self) -> Option<CaEvent> {
        None
    }
}

/// Driver that keeps every CA_PMT body, for tests and the harness.
#[derive(Clone, Debug, Default)]
pub struct RecordingDriver {
    pub caps: CaCaps,
    pub sent: Vec<Vec<u8>>,
    pub no_session: bool,
    pub fail_writes: bool,
}

impl RecordingDriver {
    /// Drains the bodies sent since the last call.
    pub fn take(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.sent)
    }
}

impl CaDriver for RecordingDriver {
    fn capabilities(&self) -> CaCaps {
        self.caps
    }

    fn has_ca_session(&self) -> bool {
        !self.no_session
    }

    fn send_ca_pmt(&mut self, body: &[u8]) -> bool {
        if self.fail_writes {
            return false;
        }
        self.sent.push(body.to_vec());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ms_psi::{CrcCheck, Pmt};
    use ms_ts::crc32_mpeg2;

    fn section(payload: &[u8]) -> Vec<u8> {
        let len = 5 + payload.len() + 4;
        let mut s = vec![0x02, 0xB0 | (len >> 8) as u8, len as u8, 0x00, 0x64];
        s.extend_from_slice(&[0xC3, 0, 0]);
        s.extend_from_slice(payload);
        let crc = crc32_mpeg2(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    #[test]
    fn service_from_pmt_dedups_caids_and_keeps_pcr() {
        let payload = [
            0xE1, 0x00, 0xF0, 0x0E, // PCR 0x100, program_info_length 14
            0x09, 0x04, 0x0B, 0x00, 0xE5, 0x73, // CA 0B00/0573
            0x09, 0x06, 0x0B, 0x00, 0xE5, 0x73, 0xAA, 0xBB, // same CAID/PID
            0x02, 0xE1, 0x01, 0xF0, 0x08, // video 0x101
            0x09, 0x06, 0x09, 0x8C, 0xE6, 0x00, 0x01, 0x02, // CA 098C/0600
            0x05, 0xE1, 0x05, 0xF0, 0x06, // private 0x105, not audio/video
            0x09, 0x04, 0x01, 0x00, 0xE7, 0x00, // ignored like C++
        ];
        let pmt = Pmt::parse(&section(&payload), CrcCheck::Verify).unwrap();
        let service = CaService::from_pmt(3, 1, 0x30, &pmt);
        assert_eq!(service.sid, 0x64);
        assert_eq!(service.pmt_pid, 0x30);
        assert_eq!(
            service.ca,
            vec![
                CaDescriptor {
                    caid: 0x0B00,
                    pid: 0x0573,
                    private_data: vec![]
                },
                CaDescriptor {
                    caid: 0x098C,
                    pid: 0x0600,
                    private_data: vec![1, 2]
                },
            ]
        );
        let pids: Vec<_> = service
            .streams
            .iter()
            .map(|s| (s.stream_type, s.pid))
            .collect();
        assert_eq!(pids, [(2, 0x101), (5, 0x105), (0, 0x100)]);
    }

    #[test]
    fn short_ca_descriptor_and_limit() {
        let mut service = CaService::default();
        service.add_ca_descriptor(&[0x0B, 0x00, 0xE0]);
        assert!(service.ca.is_empty());
        for pid in 0..30u8 {
            service.add_ca_descriptor(&[0x01, 0x00, 0xE0, pid]);
        }
        assert_eq!(service.ca.len(), MAX_CAID);
    }
}
