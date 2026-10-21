//! The CAM's program list: CA_PMT list management, quirks and the service limit.

use crate::capmt::{
    CA_ENABLE_NOT_POSSIBLE_TECHNICAL, CA_ENABLE_POSSIBLE, CMD_NOT_SELECTED, CMD_OK_DESCRAMBLING,
    CMD_OK_MMI, CMD_QUERY, CaPmt, ListManagement,
};
use crate::config::{CamConfig, Quirks};
use std::collections::BTreeSet;

/// What the CAM does with a listed program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Descrambling,
    /// Wanted, but beyond `max_services`.
    OverLimit,
    NotSelected,
    /// No command at all: nothing to descramble.
    Clear,
}

/// One program the CAM was told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub capmt: CaPmt,
    pub state: ProgramState,
    /// Time descrambling (re)started; `None` unless descrambling.
    pub active_since: Option<u64>,
    restart: bool,
}

impl Program {
    pub fn number(&self) -> u16 {
        self.capmt.program_number
    }

    fn wants_descrambling(&self) -> bool {
        matches!(
            self.capmt.program_cmd(),
            Some(CMD_OK_DESCRAMBLING | CMD_OK_MMI)
        )
    }
}

/// Result of applying one CA_PMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// The list changed (false for queries and ignored CA_PMTs).
    pub changed: bool,
    /// `CA_enable` the CAM would report for this program.
    pub ca_enable: u8,
    pub query: bool,
}

/// Same content, ignoring list management (which only says how to apply it).
fn same_content(a: &CaPmt, b: &CaPmt) -> bool {
    a.program_number == b.program_number
        && a.version == b.version
        && a.current_next == b.current_next
        && a.cmd_id == b.cmd_id
        && a.descriptors == b.descriptors
        && a.streams == b.streams
}

/// Program list state machine.
#[derive(Debug, Clone, Default)]
pub struct ProgramList {
    programs: Vec<Program>,
    max_services: usize,
    quirks: Quirks,
}

impl ProgramList {
    pub fn new(config: &CamConfig) -> Self {
        Self {
            programs: Vec::new(),
            max_services: config.max_services,
            quirks: config.quirks,
        }
    }

    pub fn programs(&self) -> &[Program] {
        &self.programs
    }

    /// Forgets every program (CA session closed or CAM reset).
    pub fn clear(&mut self) {
        self.programs.clear();
    }

    /// Applies a CA_PMT at time `now`.
    pub fn apply(&mut self, pmt: &CaPmt, now: u64) -> Applied {
        if pmt.program_cmd() == Some(CMD_QUERY) {
            return Applied {
                changed: false,
                ca_enable: self.query(pmt),
                query: true,
            };
        }
        let restart_all = self.quirks.reset_on_only;
        let changed = match pmt.list_management {
            ListManagement::Only | ListManagement::First => {
                let old = std::mem::take(&mut self.programs);
                let keep = old
                    .into_iter()
                    .find(|p| !restart_all && same_content(&p.capmt, pmt));
                self.programs.push(Self::program(pmt, keep));
                true
            }
            ListManagement::More | ListManagement::Last | ListManagement::Add => {
                self.upsert(pmt, false);
                true
            }
            ListManagement::Update if self.quirks.ignore_update => false,
            ListManagement::Update => {
                self.upsert(pmt, self.quirks.restart_on_update);
                true
            }
            ListManagement::Other(_) => false,
        };
        if changed {
            self.recompute(now);
        }
        let ca_enable = match self.find(pmt.program_number).map(|p| p.state) {
            Some(ProgramState::Descrambling) => CA_ENABLE_POSSIBLE,
            _ => CA_ENABLE_NOT_POSSIBLE_TECHNICAL,
        };
        Applied {
            changed,
            ca_enable,
            query: false,
        }
    }

    fn find(&self, number: u16) -> Option<&Program> {
        self.programs.iter().find(|p| p.number() == number)
    }

    fn upsert(&mut self, pmt: &CaPmt, force_restart: bool) {
        match self
            .programs
            .iter()
            .position(|p| p.number() == pmt.program_number)
        {
            Some(i) => {
                let old = self.programs.remove(i);
                let keep = (!force_restart && same_content(&old.capmt, pmt)).then_some(old);
                self.programs.insert(i, Self::program(pmt, keep));
            }
            None => self.programs.push(Self::program(pmt, None)),
        }
    }

    fn program(pmt: &CaPmt, keep: Option<Program>) -> Program {
        match keep {
            Some(mut p) => {
                p.capmt = pmt.clone();
                p
            }
            None => Program {
                capmt: pmt.clone(),
                state: ProgramState::Clear,
                active_since: None,
                restart: true,
            },
        }
    }

    fn recompute(&mut self, now: u64) {
        let mut active = 0;
        for p in &mut self.programs {
            let state = if p.capmt.program_cmd() == Some(CMD_NOT_SELECTED) {
                ProgramState::NotSelected
            } else if !p.wants_descrambling() {
                ProgramState::Clear
            } else if active < self.max_services {
                active += 1;
                ProgramState::Descrambling
            } else {
                ProgramState::OverLimit
            };
            p.active_since = match state {
                ProgramState::Descrambling if p.restart || p.active_since.is_none() => Some(now),
                ProgramState::Descrambling => p.active_since,
                _ => None,
            };
            p.state = state;
            p.restart = false;
        }
    }

    fn query(&self, pmt: &CaPmt) -> u8 {
        let others = self
            .programs
            .iter()
            .filter(|p| p.number() != pmt.program_number && p.state == ProgramState::Descrambling)
            .count();
        if others < self.max_services {
            CA_ENABLE_POSSIBLE
        } else {
            CA_ENABLE_NOT_POSSIBLE_TECHNICAL
        }
    }

    /// PIDs that come out clear at `now`, given the descramble start latency.
    pub fn clear_pids(&self, now: u64, latency_ms: u64) -> BTreeSet<u16> {
        let mut pids = BTreeSet::new();
        for p in &self.programs {
            let Some(since) = p.active_since else {
                continue;
            };
            if now < since.saturating_add(latency_ms) {
                continue;
            }
            for s in &p.capmt.streams {
                if matches!(
                    p.capmt.stream_cmd(s),
                    Some(CMD_OK_DESCRAMBLING | CMD_OK_MMI)
                ) {
                    pids.insert(s.pid);
                }
            }
        }
        pids
    }

    /// Program numbers currently descrambling, in list order.
    pub fn descrambling(&self) -> Vec<u16> {
        self.programs
            .iter()
            .filter(|p| p.state == ProgramState::Descrambling)
            .map(Program::number)
            .collect()
    }
}
