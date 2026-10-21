//! Desired/actual CA state reconciler, CAPMT building and the C++-compatible policy.

pub mod capmt;
pub mod device;
pub mod program;
pub mod reconciler;

pub use capmt::{BuiltCapmt, Capmt, CapmtError, capmt_content_hash, create_capmt};
pub use device::{CaDevice, CaState, SendOutcome};
pub use program::{CaDescriptor, EsStream, Program, ProgramTable, RawDescriptor, SdtInfo};
pub use reconciler::{CaPolicy, CppCompatible, Reconciler, Step};

/// `TABLES_RESULT_*` returned by a CA when a program is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TablesResult {
    Ok,
    Retry,
    NoRetry,
}

/// Static limits of a CA (PLAN §4 `CaCaps`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaCaps {
    pub max_ca_pmt: usize,
    pub multiple_pmt: usize,
}

/// Events a CA reports back to the control plane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaEvent {
    Ready,
    Gone,
}

/// Transport of CA_PMT bodies to one CAM; each send reports whether it went out.
pub trait CaDriver {
    fn capabilities(&self) -> CaCaps;
    /// A CA support session is open (`find_session_for_resource`).
    fn has_ca_session(&self) -> bool;
    /// Writes one CA_PMT APDU body; false when the write failed.
    fn send_ca_pmt(&mut self, body: &[u8]) -> bool;
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
